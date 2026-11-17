//! A TIFF export streamed into its file is the TIFF the encoder built in
//! memory before (notes: benchmark-results/mem13-2026-11-17): same strips,
//! same appended metadata directories, byte for byte.

use std::io::Cursor;

use image::{DynamicImage, ImageBuffer, ImageEncoder, Rgba};
use little_exif::{exif_tag::ExifTag, metadata::Metadata};
use rawtonic_engine::icc_profile;
use rawtonic_engine::output_color_space::OutputColorSpace;
use rawtonic_export::exif_processing::{ExportMetadata, embed_export_metadata};
use rawtonic_export::export_processing::{
    ExportSettings, TiffBitDepth, encode_export, encode_with_metadata,
};
use rawtonic_export::output::write_new_export;

/// Several strips at both depths (the encoder's strips hold about 1 MB).
fn render(bit_depth: TiffBitDepth) -> DynamicImage {
    let (width, height) = (1201, 733);
    match bit_depth {
        TiffBitDepth::Sixteen => {
            DynamicImage::ImageRgba16(ImageBuffer::from_fn(width, height, |x, y| {
                Rgba([
                    (x * 93) as u16,
                    (y * 151) as u16,
                    ((x ^ y) * 37) as u16,
                    65535,
                ])
            }))
        }
        TiffBitDepth::Eight => {
            DynamicImage::ImageRgba8(ImageBuffer::from_fn(width, height, |x, y| {
                Rgba([x as u8, y as u8, (x ^ y) as u8, 255])
            }))
        }
    }
}

fn metadata(generated: bool) -> ExportMetadata {
    let mut exif = Metadata::new();
    exif.set_tag(ExifTag::Make("Maker".into()));
    exif.set_tag(ExifTag::Artist("Jane Example".into()));
    exif.set_tag(ExifTag::ISO(vec![400]));
    ExportMetadata {
        exif,
        xmp: Some("<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"/>".to_string()),
        generated,
    }
}

/// The encoder's TIFF in memory with the directories appended to it, as
/// exports were written before they were streamed.
fn in_memory(
    image: &DynamicImage,
    bit_depth: TiffBitDepth,
    space: OutputColorSpace,
    metadata: Option<&ExportMetadata>,
) -> Vec<u8> {
    let rgb = match bit_depth {
        TiffBitDepth::Eight => DynamicImage::ImageRgb8(image.to_rgb8()),
        TiffBitDepth::Sixteen => DynamicImage::ImageRgb16(image.to_rgb16()),
    };
    let mut bytes = Cursor::new(Vec::new());
    let mut encoder = image::codecs::tiff::TiffEncoder::new(&mut bytes);
    encoder
        .set_icc_profile(icc_profile::for_space(space).to_vec())
        .unwrap();
    rgb.write_with_encoder(encoder).unwrap();
    let mut bytes = bytes.into_inner();
    if let Some(metadata) = metadata {
        embed_export_metadata(&mut bytes, "tif", metadata).unwrap();
    }
    bytes
}

#[test]
fn a_tiff_streamed_into_its_file_has_the_bytes_of_the_tiff_built_in_memory() {
    let dir = tempfile::tempdir().unwrap();
    for bit_depth in [TiffBitDepth::Eight, TiffBitDepth::Sixteen] {
        for space in [OutputColorSpace::Srgb, OutputColorSpace::ProPhoto] {
            for generated in [None, Some(false), Some(true)] {
                let metadata = || generated.map(metadata);
                let settings: ExportSettings = serde_json::from_value(serde_json::json!({
                    "jpegQuality": 85, "keepMetadata": true, "stripGps": false,
                    "colorSpace": space, "tiffBitDepth": bit_depth,
                }))
                .unwrap();
                let image = render(bit_depth);
                let expected = in_memory(&image, bit_depth, space, metadata().as_ref());

                let path = dir.path().join(format!(
                    "{bit_depth:?}-{space:?}-{}.tif",
                    generated.map_or("none", |g| if g { "generated" } else { "plain" })
                ));
                let encoded = encode_export(image.clone(), "tif", &settings, metadata()).unwrap();
                write_new_export(&path, encoded).unwrap();
                assert!(
                    std::fs::read(&path).unwrap() == expected,
                    "{}",
                    path.display()
                );

                let bytes =
                    encode_with_metadata(image.clone(), "TIFF", &settings, metadata().as_ref())
                        .unwrap();
                assert!(bytes == expected);
                let bytes = encode_export(image, "tiff", &settings, metadata())
                    .unwrap()
                    .into_bytes()
                    .unwrap();
                assert!(bytes == expected);
            }
        }
    }
    // Only the published files: no temporary file is left beside them.
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 12);
}

#[test]
fn a_tiff_export_never_replaces_an_existing_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("photo.tif");
    std::fs::write(&path, b"kept").unwrap();
    let settings: ExportSettings = serde_json::from_value(serde_json::json!({
        "jpegQuality": 85, "keepMetadata": false, "stripGps": false, "tiffBitDepth": 16,
    }))
    .unwrap();
    let encoded = encode_export(render(TiffBitDepth::Sixteen), "tif", &settings, None).unwrap();
    assert!(write_new_export(&path, encoded).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"kept");
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}
