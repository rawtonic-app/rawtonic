//! Output colour declarations must describe the export, never the source.
use image::{DynamicImage, RgbImage};
use little_exif::{exif_tag::ExifTag, filetype::FileExtension, metadata::Metadata};
use rawtonic_engine::{
    image_loader::load_image_with_orientation, output_color_space::OutputColorSpace,
};
use rawtonic_export::{
    exif_processing::export_metadata_with,
    export_processing::{ExportSettings, TiffBitDepth, encode_with_metadata},
};

fn check_format(extension: &str, file_type: FileExtension) {
    check_depth(extension, file_type, TiffBitDepth::Eight);
}

fn check_depth(extension: &str, file_type: FileExtension, depth: TiffBitDepth) {
    for source_index in [None, Some("R98"), Some("R03")] {
        check_source(extension, file_type, depth, source_index);
    }
}

fn check_source(
    extension: &str,
    file_type: FileExtension,
    depth: TiffBitDepth,
    source_index: Option<&str>,
) {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.jpg");
    let image = DynamicImage::ImageRgb8(RgbImage::from_fn(32, 24, |x, y| {
        image::Rgb([x as u8 * 7, y as u8 * 9, 90])
    }));
    image.save(&source).unwrap();
    let mut source_exif = Metadata::new();
    source_exif.set_tag(ExifTag::ColorSpace(vec![1]));
    if let Some(index) = source_index {
        source_exif.set_tag(ExifTag::InteroperabilityIndex(index.into()));
        source_exif.set_tag(ExifTag::InteroperabilityVersion(vec![48, 49, 48, 48]));
    }
    source_exif.write_to_file(&source).unwrap();

    for space in [
        OutputColorSpace::Srgb,
        OutputColorSpace::DisplayP3,
        OutputColorSpace::AdobeRgb,
        OutputColorSpace::ProPhoto,
    ] {
        let settings: ExportSettings = serde_json::from_value(serde_json::json!({
            "jpegQuality": 85, "resize": null, "keepMetadata": true, "colorSpace": space,
            "stripGps": false, "filenameTemplate": null, "watermark": null, "tiffBitDepth": depth,
        }))
        .unwrap();
        let mut metadata = export_metadata_with(
            source.to_str().unwrap(),
            extension,
            space,
            true,
            false,
            false,
            32,
            24,
            false,
            &|| None,
            &|| None,
        )
        .unwrap();
        // Export XMP is rebuilt from catalog fields; neither source colour
        // declaration is copied or written by rawtonic-metadata.
        let xmp = metadata.xmp.as_ref().unwrap();
        assert!(!xmp.contains("exif:ColorSpace") && !xmp.contains("photoshop:ICCProfile"));
        let bytes =
            encode_with_metadata(image.clone(), extension, &settings, Some(&metadata)).unwrap();
        // ICC bytes are supplied by the unchanged profile generator. AVIF
        // sRGB and JPEG XL declare colour natively rather than with an ICC.
        if extension == "png" {
            use image::ImageDecoder;
            let profile = image::codecs::png::PngDecoder::new(std::io::Cursor::new(&bytes))
                .unwrap()
                .icc_profile()
                .unwrap()
                .unwrap();
            assert_eq!(profile, rawtonic_engine::icc_profile::for_space(space));
        } else if extension != "jxl" && (extension != "avif" || space.is_wide()) {
            let profile = rawtonic_engine::icc_profile::for_space(space);
            assert!(
                bytes.windows(profile.len()).any(|part| part == profile),
                "{extension} {space:?}: unchanged ICC profile"
            );
        }
        let read = if extension == "avif" {
            exif::Reader::new()
                .read_from_container(&mut std::io::Cursor::new(&bytes))
                .unwrap()
        } else {
            let read = Metadata::new_from_vec(&bytes, file_type).unwrap();
            exif::Reader::new()
                .read_raw(read.encode().unwrap())
                .unwrap()
        };
        let color = read
            .get_field(exif::Tag::ColorSpace, exif::In::PRIMARY)
            .unwrap();
        let expected = if space == OutputColorSpace::Srgb {
            1
        } else {
            0xffff
        };
        assert_eq!(
            color.value.get_uint(0),
            Some(expected),
            "{extension} {space:?}"
        );
        let index = read.get_field(exif::Tag::InteroperabilityIndex, exif::In::PRIMARY);
        // The TIFF appender intentionally does not emit an Interop IFD.
        let expected_index = match (extension, space, source_index) {
            ("tiff", _, _) | (_, _, None) => None,
            (_, OutputColorSpace::Srgb, _) => Some("R98"),
            (_, OutputColorSpace::AdobeRgb, _) => Some("R03"),
            _ => None,
        };
        assert_eq!(
            index.map(|tag| match &tag.value {
                exif::Value::Ascii(value) => std::str::from_utf8(&value[0]).unwrap(),
                _ => panic!("wrong tag"),
            }),
            expected_index,
            "{extension} {space:?}"
        );
        if extension == "png" {
            assert!(bytes.windows(4).any(|w| w == b"eXIf"));
        }
        // Reproduce the old colour declarations with the same encoder and
        // verify metadata changes do not alter decoded pixels.
        metadata.exif.set_tag(ExifTag::ColorSpace(vec![1]));
        metadata
            .exif
            .set_tag(ExifTag::InteroperabilityIndex("R98".into()));
        let old =
            encode_with_metadata(image.clone(), extension, &settings, Some(&metadata)).unwrap();
        assert_eq!(
            load_image_with_orientation(&bytes, None)
                .unwrap()
                .to_rgb16(),
            load_image_with_orientation(&old, None).unwrap().to_rgb16(),
            "{extension} {space:?}"
        );
        if let Some(out) = std::env::var_os("RAWTONIC_COLOR_METADATA_OUT") {
            let out = std::path::PathBuf::from(out);
            std::fs::write(
                out.join(format!(
                    "{space:?}-{depth:?}-{}.{extension}",
                    source_index.unwrap_or("none")
                )),
                &bytes,
            )
            .unwrap();
            std::fs::write(
                out.join(format!(
                    "{space:?}-{depth:?}-{}-old.{extension}",
                    source_index.unwrap_or("none")
                )),
                &old,
            )
            .unwrap();
        }
    }
}

#[test]
fn jpeg_output_color_metadata() {
    check_format("jpeg", FileExtension::JPEG);
}
#[test]
fn png_output_color_metadata() {
    check_format(
        "png",
        FileExtension::PNG {
            as_zTXt_chunk: false,
        },
    );
}
#[test]
fn tiff_output_color_metadata() {
    check_format("tiff", FileExtension::TIFF);
}
#[test]
fn webp_output_color_metadata() {
    check_format("webp", FileExtension::WEBP);
}
#[test]
fn avif_output_color_metadata() {
    check_format("avif", FileExtension::HEIF);
}
#[test]
fn jxl_output_color_metadata() {
    check_format("jxl", FileExtension::JXL);
}

#[test]
fn tiff16_output_color_metadata() {
    check_depth("tiff", FileExtension::TIFF, TiffBitDepth::Sixteen);
}
