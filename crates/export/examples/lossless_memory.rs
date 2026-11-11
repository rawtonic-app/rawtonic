//! Run each format in a fresh process under /usr/bin/time -l.
//! Synthetic 36 MP RGB8; includes source generation, excludes decode/render.
use image::{DynamicImage, Rgb, RgbImage};
use rawtonic_engine::output_color_space::OutputColorSpace;
use rawtonic_export::export_processing::{TiffBitDepth, encode_image_to_bytes};
fn main() {
    let format = std::env::args().nth(1).expect("tiff or jxl");
    assert!(matches!(format.as_str(), "tiff" | "jxl" | "jxl-fast"));
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
    let bytes = if format == "jxl-fast" {
        jxl_encoder::LosslessConfig::new()
            .with_effort(1)
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
}
