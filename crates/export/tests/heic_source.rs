//! HEIC sources (iPhone, camera HEIF) exported: the pixels the loader hands
//! the renderer and the source EXIF the encoded export carries.
//! The files are written by ImageIO at test time (macOS only; other systems
//! do not decode HEIF yet, rawtonic_engine::heif).
#![cfg(target_os = "macos")]

use rawtonic_engine::heif::test_support::encode_heic;
use rawtonic_engine::output_color_space::OutputColorSpace;
use rawtonic_export::exif_processing::{export_metadata_with, read_exif};
use rawtonic_export::export_processing::{ExportSettings, encode_with_metadata};

#[test]
fn an_exported_heic_keeps_its_exif_and_is_turned_once() {
    let (width, height) = (96, 48);
    let pixels: Vec<[u16; 3]> = (0..width * height)
        .map(|i| [(i % width * 600) as u16, 30000, 12000])
        .collect();
    let bytes = encode_heic(
        width,
        height,
        &pixels,
        "kCGColorSpaceDisplayP3",
        6,
        &[
            ("{TIFF}", "Make", "Rawtonic Test"),
            ("{TIFF}", "Model", "HEIC Writer"),
            ("{Exif}", "DateTimeOriginal", "2026:11:23 10:20:30"),
        ],
    );
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("IMG_0001.HEIC");
    rawtonic_fs::atomic_write(&source, &bytes).unwrap();

    // The export as the pipeline encodes it: the loader's pixels (the
    // container's quarter turn applied) with the source's kept metadata.
    let image = rawtonic_engine::image_loader::load_image_with_orientation(&bytes, None).unwrap();
    assert_eq!((image.width(), image.height()), (48, 96));
    let settings: ExportSettings = serde_json::from_value(serde_json::json!({
        "jpegQuality": 90, "resize": null, "keepMetadata": true, "colorSpace": "srgb",
        "stripGps": false, "filenameTemplate": null, "watermark": null,
    }))
    .unwrap();
    let metadata = export_metadata_with(
        &source.to_string_lossy(),
        "jpeg",
        OutputColorSpace::Srgb,
        true,
        false,
        false,
        image.width(),
        image.height(),
        false,
        &|| None,
        &|| None,
    )
    .expect("the source's metadata");
    let exported = encode_with_metadata(image, "jpg", &settings, Some(&metadata)).unwrap();

    let decoded = image::load_from_memory(&exported).unwrap();
    assert_eq!((decoded.width(), decoded.height()), (48, 96));
    let exif = read_exif(&exported).expect("EXIF in the export");
    let text = |tag| {
        exif.get_field(tag, exif::In::PRIMARY)
            .map(|f| f.display_value().to_string())
    };
    assert_eq!(text(exif::Tag::Make).as_deref(), Some("\"Rawtonic Test\""));
    assert_eq!(text(exif::Tag::Model).as_deref(), Some("\"HEIC Writer\""));
    assert_eq!(
        text(exif::Tag::DateTimeOriginal).as_deref(),
        Some("2026-11-23 10:20:30")
    );
    // Turned once: the export's pixels are upright and its tag says so.
    let orientation = exif
        .get_field(exif::Tag::Orientation, exif::In::PRIMARY)
        .and_then(|f| f.value.get_uint(0));
    assert!(matches!(orientation, None | Some(1)), "{orientation:?}");
}
