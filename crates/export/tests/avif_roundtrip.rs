//! Rawtonic's own AVIF exports open again as sources: encoded by the export
//! encoder (with an ICC profile for wide spaces, XMP and EXIF as the export
//! writes them), decoded by the engine's image loader, compared with the
//! pixels that were exported.

use image::{DynamicImage, Rgb32FImage, RgbImage};
use rawtonic_engine::image_loader::load_image_with_orientation;
use rawtonic_engine::output_color_space::OutputColorSpace;
use rawtonic_export::export_processing::{TiffBitDepth, encode_image_to_bytes};

/// Ramps, greys and saturated colours.
fn test_image() -> DynamicImage {
    DynamicImage::ImageRgb8(RgbImage::from_fn(96, 64, |x, y| {
        let v = (x * 255 / 95) as u8;
        image::Rgb(match y / 16 {
            0 => [v, v, v],
            1 => [v, 255 - v, 60],
            2 => [230, 40 + v / 2, 30],
            _ => [20, 90, 200 - v / 2],
        })
    }))
}

fn mean_error(a: &Rgb32FImage, b: &Rgb32FImage) -> f32 {
    assert_eq!(a.dimensions(), b.dimensions());
    let sum: f32 = a
        .as_raw()
        .iter()
        .zip(b.as_raw())
        .map(|(x, y)| (x - y).abs())
        .sum();
    sum / a.as_raw().len() as f32
}

#[test]
fn exported_avif_decodes_to_the_exported_pixels() {
    let image = test_image();
    let source = image.to_rgb32f();
    for space in [
        OutputColorSpace::Srgb,
        OutputColorSpace::DisplayP3,
        OutputColorSpace::AdobeRgb,
        OutputColorSpace::ProPhoto,
    ] {
        // The source as the loader reads it: the exported pixels in the
        // export's space, converted through its profile.
        let expected = rawtonic_engine::input_profile::to_working_space(
            image.clone(),
            rawtonic_engine::icc_profile::for_space(space),
        )
        .into_rgb32f();
        // In 8-bit levels of the sRGB encoding; wider spaces spread each
        // code value further (measured 2026-10-10: sRGB 0.30/0.75, Display
        // P3 0.56/1.32, Adobe RGB 0.56/1.27, ProPhoto 1.22/2.97).
        for (quality, tolerance) in [(100, 1.5 / 255.0), (80, 4.0 / 255.0)] {
            let bytes =
                encode_image_to_bytes(&image, "avif", quality, TiffBitDepth::Eight, space, None)
                    .unwrap();
            let decoded = load_image_with_orientation(&bytes, None)
                .unwrap()
                .into_rgb32f();
            let error = mean_error(&decoded, &expected);
            assert!(
                error < tolerance,
                "{space:?} quality {quality}: mean error {:.2} levels",
                error * 255.0
            );
            if space == OutputColorSpace::Srgb {
                assert!(mean_error(&decoded, &source) < tolerance);
            }
        }
    }
}

#[test]
fn exported_avif_with_metadata_still_decodes() {
    let image = test_image();
    // TIFF-structured EXIF (orientation 6, which HEIF readers ignore) and an
    // XMP packet, as an export with metadata writes them.
    let exif: Vec<u8> = [
        b"MM\0\x2a\0\0\0\x08".as_slice(),
        &[0, 1],
        &[0x01, 0x12, 0, 3, 0, 0, 0, 1, 0, 6, 0, 0],
        &[0, 0, 0, 0],
    ]
    .concat();
    let bytes = encode_image_to_bytes(
        &image,
        "avif",
        90,
        TiffBitDepth::Eight,
        OutputColorSpace::DisplayP3,
        Some(exif),
    )
    .unwrap();
    let xmp = br#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"/></x:xmpmeta>"#;
    let bytes = rawtonic_export::avif_container::set_metadata(&bytes, None, Some(xmp)).unwrap();
    let decoded = load_image_with_orientation(&bytes, None).unwrap();
    assert_eq!((decoded.width(), decoded.height()), (96, 64));
    assert_eq!(
        rawtonic_engine::image_loader::source_orientation(&bytes, "export.avif"),
        1
    );
    let expected = rawtonic_engine::input_profile::to_working_space(
        image,
        rawtonic_engine::icc_profile::for_space(OutputColorSpace::DisplayP3),
    )
    .into_rgb32f();
    assert!(mean_error(&decoded.into_rgb32f(), &expected) < 3.0 / 255.0);
}

// --- The AI marking (EU AI Act Art. 50) and kept metadata in AVIF ---------
//
// The items are read here by a small reader of its own (not the writer's
// parser), as a HEIF reader would: `infe` entries of `iinf`, their `iloc`
// extents in the file and `cdsc` references to the primary item.

/// Top-level or child boxes: (kind, payload).
fn read_boxes(data: &[u8]) -> Vec<([u8; 4], &[u8])> {
    let mut out = Vec::new();
    let mut at = 0;
    while at + 8 <= data.len() {
        let size = u32::from_be_bytes(data[at..at + 4].try_into().unwrap()) as usize;
        let kind: [u8; 4] = data[at + 4..at + 8].try_into().unwrap();
        assert!(size >= 8 && at + size <= data.len(), "bad box size");
        out.push((kind, &data[at + 8..at + size]));
        at += size;
    }
    assert_eq!(at, data.len(), "trailing bytes");
    out
}

fn be(data: &[u8], at: &mut usize, n: usize) -> u64 {
    let v = data[*at..*at + n]
        .iter()
        .fold(0u64, |v, b| (v << 8) | u64::from(*b));
    *at += n;
    v
}

struct AvifItem {
    id: u32,
    kind: [u8; 4],
    content_type: String,
    data: Vec<u8>,
    describes: Vec<u32>,
}

/// The items of an AVIF file and its primary item ID.
fn avif_items(file: &[u8]) -> (u32, Vec<AvifItem>) {
    let top = read_boxes(file);
    let meta = top.iter().find(|(k, _)| k == b"meta").unwrap().1;
    let children = read_boxes(&meta[4..]);
    let child = |kind: &[u8; 4]| children.iter().find(|(k, _)| k == kind).map(|(_, p)| *p);

    let pitm = child(b"pitm").unwrap();
    let primary = if pitm[0] == 0 {
        be(pitm, &mut 4, 2)
    } else {
        be(pitm, &mut 4, 4)
    } as u32;

    let mut items = Vec::new();
    let iinf = child(b"iinf").unwrap();
    let mut at = 4;
    let count = be(iinf, &mut at, if iinf[0] == 0 { 2 } else { 4 });
    let entries = read_boxes(&iinf[at..]);
    assert_eq!(entries.len() as u64, count, "iinf entry count");
    for (kind, infe) in entries {
        assert_eq!(&kind, b"infe");
        assert!(infe[0] >= 2, "infe version 2 or 3");
        let mut at = 4;
        let id = be(infe, &mut at, if infe[0] == 2 { 2 } else { 4 }) as u32;
        at += 2; // protection index
        let item_type: [u8; 4] = infe[at..at + 4].try_into().unwrap();
        at += 4;
        let strings: Vec<String> = infe[at..]
            .split(|b| *b == 0)
            .map(|s| String::from_utf8_lossy(s).into_owned())
            .collect();
        let content_type = if &item_type == b"mime" {
            strings.get(1).cloned().unwrap_or_default()
        } else {
            String::new()
        };
        items.push(AvifItem {
            id,
            kind: item_type,
            content_type,
            data: Vec::new(),
            describes: Vec::new(),
        });
    }

    let iloc = child(b"iloc").unwrap();
    let version = iloc[0];
    let mut at = 4;
    let sizes = be(iloc, &mut at, 2);
    let (offset_size, length_size, base_size) = (
        (sizes >> 12) as usize,
        ((sizes >> 8) & 15) as usize,
        ((sizes >> 4) & 15) as usize,
    );
    let index_size = if version >= 1 {
        (sizes & 15) as usize
    } else {
        0
    };
    let id_size = if version < 2 { 2 } else { 4 };
    let count = be(iloc, &mut at, id_size);
    for _ in 0..count {
        let id = be(iloc, &mut at, id_size) as u32;
        let method = if version >= 1 {
            be(iloc, &mut at, 2) & 15
        } else {
            0
        };
        assert_eq!(method, 0, "file offsets only");
        assert_eq!(be(iloc, &mut at, 2), 0, "data in this file");
        let base = be(iloc, &mut at, base_size);
        let extents = be(iloc, &mut at, 2);
        let mut data = Vec::new();
        for _ in 0..extents {
            be(iloc, &mut at, index_size);
            let offset = (base + be(iloc, &mut at, offset_size)) as usize;
            let length = be(iloc, &mut at, length_size) as usize;
            data.extend_from_slice(&file[offset..offset + length]);
        }
        items.iter_mut().find(|i| i.id == id).unwrap().data = data;
    }

    if let Some(iref) = child(b"iref") {
        let id_size = if iref[0] == 0 { 2 } else { 4 };
        for (kind, reference) in read_boxes(&iref[4..]) {
            let mut at = 0;
            let from = be(reference, &mut at, id_size) as u32;
            let n = be(reference, &mut at, 2);
            let to: Vec<u32> = (0..n)
                .map(|_| be(reference, &mut at, id_size) as u32)
                .collect();
            if &kind == b"cdsc" {
                items
                    .iter_mut()
                    .find(|i| i.id == from)
                    .unwrap()
                    .describes
                    .extend(to);
            }
        }
    }
    (primary, items)
}

/// The XMP packet describing the primary item, if any.
fn primary_xmp(file: &[u8]) -> Option<String> {
    let (primary, items) = avif_items(file);
    let xmp: Vec<&AvifItem> = items
        .iter()
        .filter(|i| &i.kind == b"mime" && i.content_type == "application/rdf+xml")
        .collect();
    assert!(xmp.len() <= 1, "one XMP item at most");
    xmp.first().map(|i| {
        assert_eq!(i.describes, [primary], "XMP describes the primary item");
        String::from_utf8(i.data.clone()).unwrap()
    })
}

fn has_primary_exif(file: &[u8]) -> bool {
    let (primary, items) = avif_items(file);
    items
        .iter()
        .any(|i| &i.kind == b"Exif" && i.describes == [primary] && !i.data.is_empty())
}

fn export_settings(
    keep_metadata: bool,
    space: &str,
) -> rawtonic_export::export_processing::ExportSettings {
    serde_json::from_value(serde_json::json!({
        "jpegQuality": 85,
        "resize": null,
        "keepMetadata": keep_metadata,
        "colorSpace": space,
        "stripGps": false,
        "filenameTemplate": null,
        "watermark": null,
    }))
    .unwrap()
}

/// Exports `image` as AVIF as `save_export_image` does: metadata decided by
/// `export_metadata_with`, encoded and embedded by `encode_with_metadata`.
fn export_avif(
    source: &std::path::Path,
    keep_metadata: bool,
    generated: bool,
    space: &str,
) -> Vec<u8> {
    let image = test_image();
    let catalog = || {
        Some(rawtonic_metadata::Metadata {
            title: "Harbour at dusk".to_string(),
            creator: "Test Creator".to_string(),
            ..Default::default()
        })
    };
    let metadata = rawtonic_export::exif_processing::export_metadata_with(
        source.to_str().unwrap(),
        "avif",
        keep_metadata,
        false,
        image.width(),
        image.height(),
        generated,
        &|| None,
        &catalog,
    );
    let bytes = rawtonic_export::export_processing::encode_with_metadata(
        image,
        "avif",
        &export_settings(keep_metadata, space),
        metadata.as_ref(),
    )
    .unwrap();
    if let Some(dir) = std::env::var_os("AVIF_TEST_OUT") {
        let name = format!(
            "ai{}-keep{}-{space}.avif",
            generated as u8, keep_metadata as u8
        );
        std::fs::write(std::path::Path::new(&dir).join(name), &bytes).unwrap();
    }
    bytes
}

const SOURCE_TYPE: &str = "Iptc4xmpExt:DigitalSourceType=\"http://cv.iptc.org/newscodes/digitalsourcetype/compositeWithTrainedAlgorithmicMedia\"";

#[test]
fn avif_export_carries_the_ai_marking_and_kept_metadata() {
    let dir = tempfile::tempdir().unwrap();
    // A non-RAW source without EXIF: the kept metadata comes from the catalog.
    let source = dir.path().join("source.png");
    test_image().save(&source).unwrap();

    for space in ["srgb", "displayP3"] {
        // Generated areas, metadata stripped: rights and the marking remain.
        let bytes = export_avif(&source, false, true, space);
        let xmp = primary_xmp(&bytes).expect("AI marking needs XMP");
        assert!(xmp.contains(SOURCE_TYPE), "{space}: {xmp}");
        assert!(
            !xmp.contains("Harbour at dusk"),
            "{space}: metadata not kept"
        );
        // EXIF with the creator and creating tool, as the other formats write it.
        assert!(has_primary_exif(&bytes), "{space}: Software EXIF");
        let decoded = load_image_with_orientation(&bytes, None).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (96, 64));

        // Generated areas, metadata kept: the marking with the photo's XMP and EXIF.
        let bytes = export_avif(&source, true, true, space);
        let xmp = primary_xmp(&bytes).expect("kept metadata has XMP");
        assert!(xmp.contains(SOURCE_TYPE), "{space}: {xmp}");
        assert!(
            xmp.contains("Harbour at dusk") && xmp.contains("Test Creator"),
            "{space}: {xmp}"
        );
        assert!(has_primary_exif(&bytes), "{space}: kept EXIF");
        load_image_with_orientation(&bytes, None).unwrap();

        // No generated areas, metadata kept: the XMP without the marking.
        let bytes = export_avif(&source, true, false, space);
        let xmp = primary_xmp(&bytes).expect("kept metadata has XMP");
        assert!(!xmp.contains("DigitalSourceType"), "{space}: {xmp}");
        assert!(xmp.contains("Harbour at dusk"), "{space}: {xmp}");
        load_image_with_orientation(&bytes, None).unwrap();

        // Stripping keeps the creator, omits descriptive fields and AI marking,
        // and changes no decoded pixels.
        let bytes = export_avif(&source, false, false, space);
        let xmp = primary_xmp(&bytes).expect("creator retained");
        assert!(xmp.contains("Test Creator"));
        assert!(!xmp.contains("Harbour at dusk") && !xmp.contains("DigitalSourceType"));
        assert!(has_primary_exif(&bytes));
        let plain = encode_image_to_bytes(
            &test_image(),
            "avif",
            85,
            TiffBitDepth::Eight,
            serde_json::from_value(serde_json::json!(space)).unwrap(),
            None,
        )
        .unwrap();
        assert_eq!(
            load_image_with_orientation(&bytes, None)
                .unwrap()
                .to_rgba8(),
            load_image_with_orientation(&plain, None)
                .unwrap()
                .to_rgba8(),
            "{space}: pixels unchanged with rights metadata"
        );
    }
}
