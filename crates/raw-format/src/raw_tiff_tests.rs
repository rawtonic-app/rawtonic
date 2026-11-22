//! Tiny generated TIFFs (no photographs): raw TIFF layouts must route as
//! RAW, ordinary RGB TIFFs — even ones carrying a raw camera's make and
//! model, as editors write them — must stay raster images.

use super::{TiffKind, classify_bytes};

const SHORT: u16 = 3;
const LONG: u16 = 4;
const ASCII: u16 = 2;

#[derive(Clone)]
enum Value {
    Shorts(Vec<u16>),
    Longs(Vec<u32>),
    Text(&'static str),
    /// SubIFDs: offsets of the given IFDs (indices into `Tiff::ifds`).
    SubIfds(Vec<usize>),
}

#[derive(Clone, Default)]
struct Ifd {
    entries: Vec<(u16, Value)>,
}

impl Ifd {
    fn with(mut self, tag: u16, value: Value) -> Self {
        self.entries.push((tag, value));
        self
    }
}

/// An image IFD: size, photometric, samples (bits per sample `bits`),
/// compression, and its pixel data placed as one strip, or as tiles.
fn image(
    width: u32,
    height: u32,
    photometric: u16,
    samples: u16,
    bits: u16,
    compression: u16,
) -> Ifd {
    Ifd::default()
        .with(256, Value::Longs(vec![width]))
        .with(257, Value::Longs(vec![height]))
        .with(258, Value::Shorts(vec![bits; samples as usize]))
        .with(259, Value::Shorts(vec![compression]))
        .with(262, Value::Shorts(vec![photometric]))
        .with(277, Value::Shorts(vec![samples]))
        .with(284, Value::Shorts(vec![1]))
}

fn striped(ifd: Ifd, data_offset: u32, len: u32, rows: u32) -> Ifd {
    ifd.with(273, Value::Longs(vec![data_offset]))
        .with(278, Value::Longs(vec![rows]))
        .with(279, Value::Longs(vec![len]))
}

fn tiled(ifd: Ifd, data_offset: u32, len: u32) -> Ifd {
    ifd.with(322, Value::Longs(vec![256]))
        .with(323, Value::Longs(vec![256]))
        .with(324, Value::Longs(vec![data_offset]))
        .with(325, Value::Longs(vec![len]))
}

fn camera(ifd: Ifd, make: &'static str, model: &'static str) -> Ifd {
    ifd.with(271, Value::Text(make))
        .with(272, Value::Text(model))
}

/// Writes a little-endian TIFF: `chain` IFDs linked as pages, `subs` only
/// reachable through SubIFDs. `head` replaces bytes 8.. (e.g. a vendor
/// magic) and `pad` zero bytes of pixel data follow.
struct Tiff {
    ifds: Vec<Ifd>,
    chain: Vec<usize>,
    head: Vec<u8>,
    pad: usize,
}

impl Tiff {
    fn pages(ifds: Vec<Ifd>) -> Self {
        let chain = (0..ifds.len()).collect();
        Self {
            ifds,
            chain,
            head: Vec::new(),
            pad: 4096,
        }
    }

    fn build(&self) -> Vec<u8> {
        let mut out = b"II*\0\0\0\0\0".to_vec();
        out.extend_from_slice(&self.head);
        out.resize(out.len() + self.pad, 0);
        // Lay out IFDs: entries, then their out-of-line values.
        let mut offsets = vec![0u32; self.ifds.len()];
        let mut cursor = out.len() as u32;
        for (i, ifd) in self.ifds.iter().enumerate() {
            cursor += cursor % 2;
            offsets[i] = cursor;
            cursor += 2 + 12 * ifd.entries.len() as u32 + 4;
            for (_, value) in &ifd.entries {
                let size = value_size(value);
                if size > 4 {
                    cursor += size as u32 + (size as u32 % 2);
                }
            }
        }
        out[4..8].copy_from_slice(&offsets[self.chain[0]].to_le_bytes());
        for (i, ifd) in self.ifds.iter().enumerate() {
            if out.len() % 2 == 1 {
                out.push(0);
            }
            assert_eq!(out.len() as u32, offsets[i]);
            let mut entries = ifd.entries.clone();
            entries.sort_by_key(|(tag, _)| *tag);
            let mut extra_at = offsets[i] + 2 + 12 * entries.len() as u32 + 4;
            let mut extra = Vec::new();
            out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
            for (tag, value) in &entries {
                let (kind, count, bytes) = encode(value, &offsets);
                out.extend_from_slice(&tag.to_le_bytes());
                out.extend_from_slice(&kind.to_le_bytes());
                out.extend_from_slice(&count.to_le_bytes());
                if bytes.len() <= 4 {
                    let mut inline = bytes.clone();
                    inline.resize(4, 0);
                    out.extend_from_slice(&inline);
                } else {
                    out.extend_from_slice(&extra_at.to_le_bytes());
                    extra_at += bytes.len() as u32 + (bytes.len() as u32 % 2);
                    extra.extend_from_slice(&bytes);
                    if bytes.len() % 2 == 1 {
                        extra.push(0);
                    }
                }
            }
            let position = self.chain.iter().position(|&c| c == i);
            let next = position
                .and_then(|p| self.chain.get(p + 1))
                .map_or(0, |&n| offsets[n]);
            out.extend_from_slice(&next.to_le_bytes());
            out.extend_from_slice(&extra);
        }
        out
    }
}

fn value_size(value: &Value) -> usize {
    match value {
        Value::Shorts(v) => v.len() * 2,
        Value::Longs(v) => v.len() * 4,
        Value::Text(s) => s.len() + 1,
        Value::SubIfds(v) => v.len() * 4,
    }
}

fn encode(value: &Value, offsets: &[u32]) -> (u16, u32, Vec<u8>) {
    match value {
        Value::Shorts(v) => (
            SHORT,
            v.len() as u32,
            v.iter().flat_map(|x| x.to_le_bytes()).collect(),
        ),
        Value::Longs(v) => (
            LONG,
            v.len() as u32,
            v.iter().flat_map(|x| x.to_le_bytes()).collect(),
        ),
        Value::Text(s) => {
            let mut bytes = s.as_bytes().to_vec();
            bytes.push(0);
            (ASCII, bytes.len() as u32, bytes)
        }
        Value::SubIfds(v) => (
            LONG,
            v.len() as u32,
            v.iter().flat_map(|&i| offsets[i].to_le_bytes()).collect(),
        ),
    }
}

/// An editor's or scanner's RGB TIFF: one full-size image (data at offset
/// 8, zero pixels), optionally with a camera's make and model.
fn rgb(
    width: u32,
    height: u32,
    bits: u16,
    compression: u16,
    make_model: Option<(&'static str, &'static str)>,
) -> Ifd {
    let ifd = striped(
        image(width, height, 2, 3, bits, compression),
        8,
        4096,
        height,
    );
    match make_model {
        Some((make, model)) => camera(ifd, make, model),
        None => ifd,
    }
}

/// Makes and models whose raw files the decoder reads from TIFF containers.
const RAW_CAMERAS: &[(&str, &str)] = &[
    ("Canon", "Canon EOS-1D"),
    ("Canon", "Canon EOS-1DS"),
    ("Canon", "Canon EOS 5D Mark II"),
    ("Phase One A/S", "IQ140"),
    ("Phase One A/S", "P45"),
    ("Kodak", "DCS560C"),
    ("Kodak", "DCS520C"),
    ("KODAK", "DCS460D         FILE VERSION 3"),
    ("Canon", "EOS D2000C"),
    ("NIKON CORPORATION", "NIKON D800"),
    ("SONY", "ILCE-7M3"),
    ("Hasselblad", "H6D-100c"),
    ("PENTAX", "PENTAX K-1"),
    ("OLYMPUS CORPORATION", "E-M1"),
    ("Panasonic", "DC-G9"),
    ("FUJIFILM", "X-T3"),
    ("Leaf", "Aptus 75"),
];

#[test]
fn ordinary_rgb_tiffs_stay_raster_images() {
    let mut files = Vec::new();
    for &(bits, compression) in &[
        (8, 1),
        (16, 1),
        (8, 5),
        (16, 5),
        (8, 8),
        (16, 8),
        (16, 32946),
    ] {
        files.push((
            "striped",
            Tiff::pages(vec![rgb(4000, 3000, bits, compression, None)]),
        ));
        files.push((
            "tiled",
            Tiff::pages(vec![tiled(
                image(4000, 3000, 2, 3, bits, compression),
                8,
                4096,
            )]),
        ));
    }
    // Grayscale (scanner), palette and CMYK.
    files.push((
        "gray",
        Tiff::pages(vec![striped(image(3000, 2000, 1, 1, 16, 1), 8, 4096, 2000)]),
    ));
    files.push((
        "palette",
        Tiff::pages(vec![striped(image(300, 200, 3, 1, 8, 1), 8, 4096, 200)]),
    ));
    files.push((
        "cmyk",
        Tiff::pages(vec![striped(image(3000, 2000, 5, 4, 8, 5), 8, 4096, 2000)]),
    ));
    // Multi-page: a full image followed by a reduced one and a second page.
    files.push((
        "multi-page",
        Tiff::pages(vec![
            rgb(4000, 3000, 16, 8, None),
            rgb(400, 300, 8, 1, None).with(254, Value::Longs(vec![1])),
            rgb(2000, 1500, 8, 5, None),
        ]),
    ));
    for (name, tiff) in &files {
        assert_eq!(classify_bytes(&tiff.build()), TiffKind::Raster, "{name}");
    }
}

#[test]
fn rgb_tiffs_with_a_raw_cameras_make_and_model_stay_raster_images() {
    for &(make, model) in RAW_CAMERAS {
        for &(bits, compression) in &[(8, 1), (16, 5), (16, 8)] {
            let full = Tiff::pages(vec![rgb(
                5472,
                3648,
                bits,
                compression,
                Some((make, model)),
            )]);
            assert_eq!(
                classify_bytes(&full.build()),
                TiffKind::Raster,
                "{make} {model} {bits}-bit"
            );
        }
        // A reduced page after the full image, as some editors write.
        let pages = Tiff::pages(vec![
            rgb(5472, 3648, 16, 8, Some((make, model))),
            rgb(256, 171, 8, 1, None).with(254, Value::Longs(vec![1])),
        ]);
        assert_eq!(
            classify_bytes(&pages.build()),
            TiffKind::Raster,
            "{make} {model} pages"
        );
    }
}

#[test]
fn not_a_tiff_or_a_broken_one_stays_raster() {
    assert_eq!(classify_bytes(b""), TiffKind::Raster);
    assert_eq!(classify_bytes(b"\xff\xd8\xff\xe0 JFIF"), TiffKind::Raster);
    assert_eq!(classify_bytes(b"II*\0\xff\xff\xff\x7f"), TiffKind::Raster);
}

#[test]
fn a_phase_one_iiq_container_routes_as_raw() {
    // Phase One's TIFF-wrapped IIQ: RGB thumbnails in the TIFF chain, the
    // IIQ directory magic at byte 8. Even when its IIQ directory is
    // unreadable, the file must not open as its thumbnail.
    let mut tiff = Tiff::pages(vec![
        camera(rgb(208, 156, 8, 1, None), "Phase One A/S", "P30"),
        rgb(208, 156, 8, 1, None).with(254, Value::Longs(vec![1])),
    ]);
    tiff.head = b"IIIICwaR\0\0\0\0\0\0\0\0".to_vec();
    assert!(classify_bytes(&tiff.build()).is_raw());
}

#[test]
fn an_unmarked_camera_raw_tiff_routes_as_raw() {
    // The Kodak DCS460D decoder finds the sensor strip by NewSubfileType
    // and linearizes its samples with the GrayResponseCurve in IFD0.
    let preview_bytes = 16 * 16 * 3;
    let sensor_bytes = 128 * 96;
    let preview = camera(
        striped(image(16, 16, 2, 3, 8, 1), 8, preview_bytes, 16),
        "KODAK",
        "DCS460D         FILE VERSION 3",
    )
    .with(254, Value::Longs(vec![1]))
    .with(291, Value::Shorts((0..256).collect()))
    .with(330, Value::SubIfds(vec![1]));
    // BlackIsZero and no CFA, DNG or LinearRaw tags. The classifier must
    // recognize the decoder's sensor image behind the small RGB preview.
    let sensor = striped(
        image(128, 96, 1, 1, 8, 1),
        8 + preview_bytes,
        sensor_bytes,
        96,
    )
    .with(254, Value::Longs(vec![0]));
    let tiff = Tiff {
        ifds: vec![preview, sensor],
        chain: vec![0],
        head: Vec::new(),
        pad: (preview_bytes + sensor_bytes) as usize,
    };
    assert_eq!(classify_bytes(&tiff.build()), TiffKind::Raw);
}

#[test]
fn a_cfa_image_behind_a_thumbnail_routes_as_raw() {
    // Kodak DCS 520C/560C layout: an RGB thumbnail in IFD0, the colour
    // filter array image in a SubIFD.
    for (make, model) in [("Kodak", "DCS560C"), ("Unknown maker", "Unknown back")] {
        let thumb = camera(rgb(156, 104, 8, 1, None), make, model)
            .with(254, Value::Longs(vec![1]))
            .with(330, Value::SubIfds(vec![1]));
        let cfa = striped(image(3048, 2016, 32803, 1, 12, 7), 8, 4096, 2016)
            .with(254, Value::Longs(vec![0]))
            .with(33421, Value::Shorts(vec![2, 2]));
        let tiff = Tiff {
            ifds: vec![thumb, cfa],
            chain: vec![0],
            head: Vec::new(),
            pad: 4096,
        };
        assert!(classify_bytes(&tiff.build()).is_raw(), "{make} {model}");
    }
    // Without a decoder for the camera it is refused as a RAW, not shown
    // as its thumbnail.
    let thumb = camera(rgb(156, 104, 8, 1, None), "Unknown maker", "Unknown back")
        .with(330, Value::SubIfds(vec![1]));
    let cfa = striped(image(3048, 2016, 32803, 1, 12, 1), 8, 4096, 2016);
    let tiff = Tiff {
        ifds: vec![thumb, cfa],
        chain: vec![0],
        head: Vec::new(),
        pad: 4096,
    };
    assert_eq!(classify_bytes(&tiff.build()), TiffKind::UnsupportedRaw);
}

/// Classifies every `.tif`/`.tiff` in `RAWTONIC_TIFF_SAMPLES` and prints its
/// kind and the time the check took (cold, without the cache), for checking
/// real raw and RGB TIFFs that stay out of git:
/// `RAWTONIC_TIFF_SAMPLES=DIR cargo test -p rawtonic-raw-format --release --lib -- --ignored --nocapture`
#[test]
#[ignore = "needs RAWTONIC_TIFF_SAMPLES"]
fn classify_sample_folder() {
    let directory = std::env::var("RAWTONIC_TIFF_SAMPLES").expect("RAWTONIC_TIFF_SAMPLES");
    let mut paths: Vec<_> = std::fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| {
                    ext.eq_ignore_ascii_case("tif") || ext.eq_ignore_ascii_case("tiff")
                })
        })
        .collect();
    paths.sort();
    for path in paths {
        let start = std::time::Instant::now();
        let kind = super::classify(&path);
        let micros = start.elapsed().as_micros();
        println!(
            "{}\t{kind:?}\t{micros} us",
            path.file_name().unwrap().to_string_lossy()
        );
    }
}
