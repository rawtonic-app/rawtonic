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
