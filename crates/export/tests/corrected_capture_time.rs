//! A corrected capture time (Shift Capture Time) reaches the export's EXIF
//! on every way the export reads the source's metadata; without a
//! correction nothing changes.
use image::{DynamicImage, RgbImage};
use little_exif::{exif_tag::ExifTag, filetype::FileExtension, metadata::Metadata};
use rawtonic_engine::output_color_space::OutputColorSpace;
use rawtonic_export::{
    exif_processing::{CAPTURE_CORRECTED, export_metadata_with},
    export_processing::{ExportSettings, TiffBitDepth, encode_with_metadata},
};
use std::collections::HashMap;
use std::path::Path;

fn image() -> DynamicImage {
    DynamicImage::ImageRgb8(RgbImage::from_fn(16, 12, |x, y| {
        image::Rgb([x as u8 * 9, y as u8 * 11, 70])
    }))
}

/// A camera JPEG with its capture time, fraction and offset.
fn camera_jpeg(path: &Path) {
    image().save(path).unwrap();
    let mut exif = Metadata::new();
    exif.set_tag(ExifTag::Make("Maker".into()));
    exif.set_tag(ExifTag::DateTimeOriginal("2026:10:30 23:30:00".into()));
    exif.set_tag(ExifTag::SubSecTimeOriginal("05".into()));
    exif.set_tag(ExifTag::OffsetTimeOriginal("+02:00".into()));
    exif.set_tag(ExifTag::CreateDate("2026:10:30 23:30:00".into()));
    exif.write_to_file(path).unwrap();
}

fn map(entries: &[(&str, &str)]) -> HashMap<String, String> {
    entries
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// The encoded export of `source` with `exif` as its map.
fn encoded(source: &Path, exif: Option<HashMap<String, String>>) -> Vec<u8> {
    let metadata = export_metadata_with(
        source.to_str().unwrap(),
        "jpeg",
        OutputColorSpace::Srgb,
        true,
        true,
        false,
        16,
        12,
        false,
        &|| exif.clone(),
        &|| None,
    )
    .unwrap();
    let settings: ExportSettings = serde_json::from_value(serde_json::json!({
        "jpegQuality": 85, "resize": null, "keepMetadata": true, "colorSpace": "srgb",
        "stripGps": true, "filenameTemplate": null, "watermark": null,
        "tiffBitDepth": TiffBitDepth::Eight,
    }))
    .unwrap();
    encode_with_metadata(image(), "jpeg", &settings, Some(&metadata)).unwrap()
}

/// The capture fields of an export of `source` with `exif` as its map.
fn exported(source: &Path, exif: Option<HashMap<String, String>>) -> [Option<String>; 4] {
    let bytes = encoded(source, exif);
    let read = Metadata::new_from_vec(&bytes, FileExtension::JPEG).unwrap();
    let read = exif::Reader::new()
        .read_raw(read.encode().unwrap())
        .unwrap();
    let text = |tag| {
        read.get_field(tag, exif::In::PRIMARY)
            .map(|f| f.display_value().to_string().trim_matches('"').to_string())
    };
    [
        text(exif::Tag::DateTimeOriginal),
        text(exif::Tag::SubSecTimeOriginal),
        text(exif::Tag::OffsetTimeOriginal),
        text(exif::Tag::DateTimeDigitized),
    ]
}

#[test]
fn a_jpeg_source_copied_whole_takes_the_corrected_time() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("camera.jpg");
    camera_jpeg(&source);
    let before = exported(&source, None);
    assert_eq!(before[0].as_deref(), Some("2026-10-30 23:30:00"));
    assert_eq!(before[1].as_deref(), Some("05"));
    assert_eq!(before[2].as_deref(), Some("+02:00"));
    // The library's map without a correction changes nothing.
    let plain = map(&[("DateTimeOriginal", "2026-10-30 23:30:00")]);
    assert_eq!(exported(&source, Some(plain)), before);
    let corrected = map(&[
        ("DateTimeOriginal", "2026-10-31 00:30:00"),
        ("SubSecTimeOriginal", "05"),
        ("OffsetTimeOriginal", "+02:00"),
        (CAPTURE_CORRECTED, "1"),
    ]);
    let after = exported(&source, Some(corrected));
    assert_eq!(after[0].as_deref(), Some("2026-10-31 00:30:00"));
    assert_eq!(after[1].as_deref(), Some("05"));
    assert_eq!(after[2].as_deref(), Some("+02:00"));
    // Other dates stay as the file has them.
    assert_eq!(after[3], before[3]);
}

#[test]
fn a_raw_source_read_through_its_map_takes_the_corrected_time() {
    let dir = tempfile::tempdir().unwrap();
    // Not decodable: the export reads the library's map, as for a RAW.
    let source = dir.path().join("camera.ARW");
    std::fs::write(&source, b"raw").unwrap();
    let plain = map(&[
        ("DateTimeOriginal", "2026-10-30 23:30:00"),
        ("Make", "Maker"),
    ]);
    let before = exported(&source, Some(plain));
    assert_eq!(before[0].as_deref(), Some("2026-10-30 23:30:00"));
    assert_eq!(before[1], None, "no fraction is added without a correction");
    let corrected = map(&[
        ("DateTimeOriginal", "2024-02-29 00:00:01"),
        ("SubSecTimeOriginal", "250"),
        ("Make", "Maker"),
        (CAPTURE_CORRECTED, "1"),
    ]);
    let after = exported(&source, Some(corrected));
    assert_eq!(after[0].as_deref(), Some("2024-02-29 00:00:01"));
    assert_eq!(after[1].as_deref(), Some("250"));
    assert_eq!(after[2], None, "no offset is invented");
}

#[test]
fn a_correction_changes_only_metadata_never_pixels() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("camera.jpg");
    camera_jpeg(&source);
    let corrected = map(&[
        ("DateTimeOriginal", "2026-10-31 00:30:00"),
        ("SubSecTimeOriginal", "05"),
        (CAPTURE_CORRECTED, "1"),
    ]);
    let (plain, shifted) = (encoded(&source, None), encoded(&source, Some(corrected)));
    assert_ne!(plain, shifted, "the EXIF differs");
    let pixels = |bytes: &[u8]| image::load_from_memory(bytes).unwrap().to_rgb8().into_raw();
    assert_eq!(pixels(&plain), pixels(&shifted));
}
