//! Run each format in a fresh process under /usr/bin/time -l.
//! Peak RSS comes from time, not the printed encode timer.
//! Synthetic 36 MP RGB8; RSS includes generation/input/encoder, excludes RAW/GPU.
//! `--rgba` starts from the RGBA8 frame an 8-bit export render returns (TIFF
//! drops its alpha in place first, as `encode_with_metadata` does);
//! `--concurrent N` encodes N such frames at once, as a batch export can.
//! Optional decoder verification adds decode memory: do not use it for peak-RSS comparisons.
use image::{DynamicImage, Rgb, RgbImage};
use rawtonic_engine::output_color_space::OutputColorSpace;
use rawtonic_export::export_processing::{
    TiffBitDepth, drop_alpha_in_place, encode_image_to_bytes,
};

fn frame(rgba: bool) -> DynamicImage {
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
    if rgba {
        DynamicImage::ImageRgba8(image.to_rgba8())
    } else {
        image
    }
}

fn encode(format: &str, effort: Option<u8>, image: DynamicImage) -> Vec<u8> {
    if let Some(effort) = effort {
        let converted;
        let rgb = match &image {
            DynamicImage::ImageRgb8(rgb) => rgb,
            other => {
                converted = other.to_rgb8();
                &converted
            }
        };
        return jxl_encoder::LosslessConfig::new()
            .with_effort(effort)
            .encode(
                rgb.as_raw(),
                rgb.width(),
                rgb.height(),
                jxl_encoder::PixelLayout::Rgb8,
            )
            .unwrap();
    }
    let image = if format == "tiff" {
        drop_alpha_in_place(image)
    } else {
        image
    };
    encode_image_to_bytes(
        &image,
        format,
        100,
        TiffBitDepth::Eight,
        OutputColorSpace::Srgb,
        None,
    )
    .unwrap()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let format = args
        .get(1)
        .expect("tiff, jxl, or jxl-effort [1..10]")
        .clone();
    assert!(matches!(format.as_str(), "tiff" | "jxl" | "jxl-effort"));
    let effort = (format == "jxl-effort").then(|| args[2].parse::<u8>().expect("effort"));
    let rgba = args.iter().any(|arg| arg == "--rgba");
    let concurrent = args
        .iter()
        .position(|arg| arg == "--concurrent")
        .map(|at| args[at + 1].parse::<usize>().unwrap());
    if let Some(count) = concurrent {
        let frames: Vec<DynamicImage> = (0..count).map(|_| frame(rgba)).collect();
        let start = std::time::Instant::now();
        let sizes: Vec<usize> = std::thread::scope(|scope| {
            let jobs: Vec<_> = frames
                .into_iter()
                .map(|image| scope.spawn(|| encode(&format, effort, image).len()))
                .collect();
            jobs.into_iter().map(|job| job.join().unwrap()).collect()
        });
        println!(
            "format={format} concurrent={count} rgba={rgba} bytes={sizes:?} encode_seconds={:.3}",
            start.elapsed().as_secs_f64()
        );
        return;
    }
    let image = frame(rgba);
    let source = (args.iter().any(|arg| arg.starts_with("--verify"))).then(|| image.to_rgb8());
    let pixels = image.width() * image.height();
    let start = std::time::Instant::now();
    let bytes = encode(&format, effort, image);
    println!(
        "format={format} rgba={rgba} pixels={pixels} bytes={} encode_seconds={:.3}",
        bytes.len(),
        start.elapsed().as_secs_f64()
    );
    if let Some(path) = std::env::var_os("EXQ_JXL_OUTPUT") {
        rawtonic_export::output::write_new_output(path, &bytes).unwrap();
    }
    if args.iter().any(|arg| arg == "--verify-libjxl") {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("export.jxl");
        rawtonic_export::output::write_new_output(&path, &bytes).unwrap();
        let decoded = std::process::Command::new("djxl")
            .arg(&path)
            .args(["-", "--output_format", "ppm"])
            .output()
            .expect("requires libjxl's djxl decoder");
        assert!(
            decoded.status.success(),
            "{}",
            String::from_utf8_lossy(&decoded.stderr)
        );
        let decoded = image::load_from_memory(&decoded.stdout).unwrap();
        assert_eq!(&decoded.to_rgb8(), source.as_ref().unwrap());
        println!("libjxl_decoded_pixels_identical=true");
    }
    if args.iter().any(|arg| arg == "--verify-rust-reader") {
        let decoder = jxl_oxide::integration::JxlDecoder::new(std::io::Cursor::new(bytes)).unwrap();
        let decoded = DynamicImage::from_decoder(decoder).unwrap();
        assert_eq!(&decoded.to_rgb8(), source.as_ref().unwrap());
        println!("decoded_pixels_identical=true");
    }
}
