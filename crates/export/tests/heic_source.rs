//! HEIC sources (iPhone, camera HEIF) as the export reads them: the EXIF it
//! carries into an export, and the pixels the loader hands the renderer.
//! The files are written by ImageIO at test time (macOS only; other systems
//! do not decode HEIF yet, rawtonic_engine::heif).
#![cfg(target_os = "macos")]

use rawtonic_engine::heif::test_support::encode_heic;

#[test]
fn heic_exif_reaches_the_export_and_its_orientation_is_applied_once() {
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
    let exif = rawtonic_export::exif_processing::read_exif(&bytes).expect("EXIF in the HEIC");
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
    // The loader applies the container's quarter turn; the export then
    // writes Orientation 1 (exif_processing), so it is not turned twice.
    let image = rawtonic_engine::image_loader::load_image_with_orientation(&bytes, None).unwrap();
    assert_eq!((image.width(), image.height()), (48, 96));
}
