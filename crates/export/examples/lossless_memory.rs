//! Run each format in a fresh process under /usr/bin/time -l.
//! Synthetic 36 MP RGB8; includes source generation, excludes decode/render.
use image::{DynamicImage, Rgb, RgbImage};
use rawtonic_engine::output_color_space::OutputColorSpace;
use rawtonic_export::export_processing::{TiffBitDepth, encode_image_to_bytes};
fn main() {
    let format = std::env::args()
        .nth(1)
        .expect("tiff, jxl, or jxl-effort [1..10]");
    assert!(matches!(format.as_str(), "tiff" | "jxl" | "jxl-effort"));
    let mut state = 123456789u32;
    let image = DynamicImage::ImageRgb8(RgbImage::from_fn(7360, 4912, |x, y| {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        let noise = (state & 15) as u8;
        Rgb([
            ((x * 255 / 7360) as u8).saturating_add(noise),
            ((y * 255 / 4912) as u8).saturating_add(noise),
            (((x + y) * 255 / 12272) as u8).saturating_add(noise),
        ])
    }));
    let start = std::time::Instant::now();
    let bytes = if format == "jxl-effort" {
        jxl_encoder::LosslessConfig::new()
            .with_effort(std::env::args().nth(2).expect("effort").parse().unwrap())
            .encode(
                image.as_bytes(),
                image.width(),
                image.height(),
                jxl_encoder::PixelLayout::Rgb8,
            )
            .unwrap()
    } else {
        encode_image_to_bytes(
            &image,
            &format,
            100,
            TiffBitDepth::Eight,
            OutputColorSpace::Srgb,
            None,
        )
        .unwrap()
    };
    println!(
        "format={format} pixels={} bytes={} encode_seconds={:.3}",
        image.width() * image.height(),
        bytes.len(),
        start.elapsed().as_secs_f64()
    );
    if let Some(path) = std::env::var_os("EXQ_JXL_OUTPUT") {
        rawtonic_export::output::write_new_output(path, &bytes).unwrap();
    }
    if std::env::args().any(|arg| arg == "--verify-libjxl") {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("export.jxl");
        rawtonic_export::output::write_new_output(&source, &bytes).unwrap();
        let decoded = std::process::Command::new("djxl")
            .arg(&source)
            .args(["-", "--output_format", "ppm"])
            .output()
            .expect("requires libjxl's djxl decoder");
        assert!(
            decoded.status.success(),
            "{}",
            String::from_utf8_lossy(&decoded.stderr)
        );
        let decoded = image::load_from_memory(&decoded.stdout).unwrap();
        assert_eq!(decoded.to_rgb8(), image.to_rgb8());
        println!("libjxl_decoded_pixels_identical=true");
    }
    if std::env::args().any(|arg| arg == "--verify") {
        let decoder = jxl_oxide::integration::JxlDecoder::new(std::io::Cursor::new(bytes)).unwrap();
        let decoded = DynamicImage::from_decoder(decoder).unwrap();
        assert_eq!(decoded.to_rgb8(), image.to_rgb8());
        println!("decoded_pixels_identical=true");
    }
}
