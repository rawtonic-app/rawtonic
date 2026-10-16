//! Capture facts of a RAW the RAW decoder reads (a DNG made here, the
//! kind `rawtonic merge` writes: data first, its IFDs at the end), so the
//! decoder path of `capture::read_capture` is covered without a camera file.
use rawtonic_naming::DateSource;
use std::fs;

fn dng(width: u32, height: u32) -> Vec<u8> {
    let mut metadata = rawler::decoders::RawMetadata {
        make: "Testmaker".into(),
        model: "RAW One".into(),
        ..Default::default()
    };
    metadata.exif.date_time_original = Some("2024:07:14 06:15:30".into());
    metadata.exif.sub_sec_time_original = Some("5".into());
    metadata.exif.offset_time_original = Some("-04:00".into());
    metadata.exif.modify_date = Some("2024:07:14 06:15:31".into());
    metadata.exif.iso_speed_ratings = Some(200);
    metadata.exif.fnumber = Some(rawler::formats::tiff::Rational::new(56, 10));
    metadata.exif.exposure_time = Some(rawler::formats::tiff::Rational::new(1, 1000));
    metadata.exif.focal_length = Some(rawler::formats::tiff::Rational::new(85, 1));
    metadata.exif.serial_number = Some("SN 42".into());
    metadata.exif.lens_model = Some("85mm F1.4".into());
    // Noise, so the deflated data is about as large as the pixels.
    let mut seed = 0x2545_f491_u32;
    let image = image::Rgb32FImage::from_fn(width, height, |_, _| {
        image::Rgb(std::array::from_fn(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            (seed % 1000) as f32 / 1000.0
        }))
    });
    rawtonic_merge::dng::encode_linear_dng(
        &image,
        &rawtonic_merge::dng::DngMeta {
            color: None,
            metadata: Some(&metadata),
            keep_lens: true,
            baseline_exposure: None,
            preview: None,
            software: "test",
        },
    )
    .unwrap()
}

#[test]
fn reads_capture_facts_of_a_raw_through_the_raw_decoder() {
    let d = tempfile::tempdir().unwrap();
    // Small, and larger than the first read step (its IFDs past 1 MiB).
    for (name, size) in [("small.dng", 32), ("large.dng", 640)] {
        let path = d.path().join(name);
        let bytes = dng(size, size);
        fs::write(&path, &bytes).unwrap();
        if size > 32 {
            assert!(bytes.len() > 1 << 20, "{}", bytes.len());
        }
        let capture = rawtonic_ingest::capture::read_capture(&path);
        assert_eq!(capture.date_source, DateSource::Original, "{name}");
        assert_eq!(
            capture.captured.map(|d| d.to_string()).as_deref(),
            Some("2024-07-14 06:15:30"),
            "{name}"
        );
        assert_eq!(capture.subsec.as_deref(), Some("5"));
        assert_eq!(capture.offset.as_deref(), Some("-04:00"));
        assert_eq!(capture.make.as_deref(), Some("Testmaker"));
        assert_eq!(capture.model.as_deref(), Some("RAW One"));
        assert_eq!(capture.serial.as_deref(), Some("SN 42"));
        assert_eq!(capture.lens.as_deref(), Some("85mm F1.4"));
        assert_eq!(capture.iso, Some(200));
        assert_eq!(capture.aperture, Some(5.6));
        assert_eq!(capture.focal_length, Some(85.0));
        assert_eq!(capture.exposure, Some((1, 1000)));
    }
}
