//! IPTC fields imported from real editor packets reach encoded exports.
use image::{DynamicImage, RgbImage};
use rawtonic_engine::output_color_space::OutputColorSpace;
use rawtonic_export::{
    exif_processing::export_metadata_with,
    export_processing::{ExportSettings, encode_with_metadata},
};

#[test]
fn imported_iptc_extension_is_embedded_and_generated_marking_stays_authoritative() {
    let source = tempfile::tempdir().unwrap();
    let path = source.path().join("source.jpg");
    let image = DynamicImage::ImageRgb8(RgbImage::from_pixel(32, 24, image::Rgb([90, 120, 150])));
    image.save(&path).unwrap();
    let catalog = rawtonic_metadata::read_xmp(include_str!(
        "../../metadata/tests/fixtures/lightroom-classic-15.6-de.xmp"
    ))
    .unwrap()
    .0;
    let settings: ExportSettings = serde_json::from_value(serde_json::json!({
        "jpegQuality": 85, "resize": null, "keepMetadata": true,
        "colorSpace": "srgb", "stripGps": false, "filenameTemplate": null, "watermark": null,
    }))
    .unwrap();
    for generated in [false, true] {
        let metadata = export_metadata_with(
            path.to_str().unwrap(),
            "jpeg",
            OutputColorSpace::Srgb,
            true,
            false,
            true,
            32,
            24,
            generated,
            &|| None,
            &|| Some(catalog.clone()),
        )
        .unwrap();
        let xmp = metadata.xmp.as_ref().unwrap();
        let read = rawtonic_metadata::read_xmp(xmp).unwrap().0;
        assert_eq!(read.iptc["personInImage"], "Person A\nPerson B");
        assert_eq!(read.iptc["event"], "Test Event 2026");
        assert_eq!(
            read.iptc["digitalSourceType"],
            if generated {
                rawtonic_metadata::DIGITAL_SOURCE_COMPOSITE_AI
            } else {
                "Original digital capture"
            }
        );
        let bytes =
            encode_with_metadata(image.clone(), "jpeg", &settings, Some(&metadata)).unwrap();
        assert!(bytes.windows(xmp.len()).any(|w| w == xmp.as_bytes()));
    }
}
