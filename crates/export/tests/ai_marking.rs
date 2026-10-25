//! A photo with generated areas never leaves without its AI marking (IPTC
//! Digital Source Type), whatever happens to the rest of its metadata.

use image::{DynamicImage, RgbImage};
use rawtonic_export::exif_processing::{ExportMetadata, export_metadata_with};
use rawtonic_export::export_processing::{ExportSettings, encode_with_metadata};

const MARKING: &str = "compositeWithTrainedAlgorithmicMedia";

fn settings() -> ExportSettings {
    serde_json::from_value(serde_json::json!({
        "jpegQuality": 85, "resize": null, "keepMetadata": true, "colorSpace": "srgb",
        "stripGps": false, "filenameTemplate": null, "watermark": null,
    }))
    .unwrap()
}

fn image() -> DynamicImage {
    DynamicImage::ImageRgb8(RgbImage::from_fn(32, 24, |x, y| {
        image::Rgb([x as u8 * 8, y as u8 * 10, 90])
    }))
}

fn carries_marking(bytes: &[u8]) -> bool {
    bytes
        .windows(MARKING.len())
        .any(|w| w == MARKING.as_bytes())
}

#[test]
fn kept_metadata_of_a_missing_original_still_marks_generated_areas() {
    let metadata = export_metadata_with(
        "/nonexistent/photo.jpg",
        "jpeg",
        true,
        false,
        false,
        32,
        24,
        true,
        &|| None,
        &|| None,
    )
    .expect("the marking without the original");
    assert!(metadata.generated && metadata.xmp.unwrap().contains(MARKING));
}

#[test]
fn a_marking_that_does_not_fit_falls_back_to_the_marking_alone() {
    // XMP larger than one JPEG segment: the full metadata cannot be embedded.
    let mut metadata = ExportMetadata::marking_only().unwrap();
    let xmp = metadata.xmp.take().unwrap();
    metadata.xmp = Some(xmp.replace(
        "<rdf:RDF",
        &format!("<!--{}--><rdf:RDF", "x".repeat(70_000)),
    ));
    let bytes = encode_with_metadata(image(), "jpg", &settings(), Some(&metadata)).unwrap();
    assert!(carries_marking(&bytes));
    image::load_from_memory(&bytes).expect("a readable JPEG");

    // Without generated areas the export is written without the metadata.
    metadata.generated = false;
    let bytes = encode_with_metadata(image(), "jpg", &settings(), Some(&metadata)).unwrap();
    assert!(!carries_marking(&bytes));
}

#[test]
fn a_source_exported_with_generated_areas_keeps_its_marking() {
    // An export with generated areas, brought back (Edit With) and exported
    // again with a recipe of its own that has none.
    let tmp = tempfile::tempdir().unwrap();
    let marked = ExportMetadata::marking_only().unwrap();
    let bytes = encode_with_metadata(image(), "tiff", &settings(), Some(&marked)).unwrap();
    let render = tmp.path().join("A_edit.tiff");
    std::fs::write(&render, &bytes).unwrap();
    assert!(rawtonic_metadata::file_marked_generated(&render));
    let plain = tmp.path().join("B.tiff");
    std::fs::write(
        &plain,
        encode_with_metadata(image(), "tiff", &settings(), None).unwrap(),
    )
    .unwrap();
    assert!(!rawtonic_metadata::file_marked_generated(&plain));

    for keep in [true, false] {
        let generated = rawtonic_metadata::file_marked_generated(&render);
        let metadata = export_metadata_with(
            render.to_str().unwrap(),
            "jpeg",
            keep,
            false,
            false,
            32,
            24,
            generated,
            &|| None,
            &|| None,
        )
        .expect("metadata with the marking");
        let out = encode_with_metadata(image(), "jpg", &settings(), Some(&metadata)).unwrap();
        assert!(carries_marking(&out), "keep metadata {keep}");
    }
}
