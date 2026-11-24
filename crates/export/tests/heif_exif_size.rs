//! HEIF Exif items are not limited to the 64 KiB of a JPEG APP1 segment
//! (a Xiaomi phone writes 192 KB). The files are built here, byte by byte,
//! so the test does not depend on a system HEIF encoder.

use rawtonic_export::exif_processing::read_exif;

fn boxed(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut out = ((body.len() + 8) as u32).to_be_bytes().to_vec();
    out.extend_from_slice(kind);
    out.extend_from_slice(body);
    out
}

/// A little-endian TIFF with Make, Model, DateTimeOriginal and a MakerNote
/// of `padding` bytes (the part that makes an item large).
fn tiff(padding: usize) -> Vec<u8> {
    fn entry(tag: u16, kind: u16, count: u32, value: u32) -> Vec<u8> {
        let mut e = tag.to_le_bytes().to_vec();
        e.extend_from_slice(&kind.to_le_bytes());
        e.extend_from_slice(&count.to_le_bytes());
        e.extend_from_slice(&value.to_le_bytes());
        e
    }
    let make = b"Rawtonic Test\0";
    let model = b"Big EXIF\0";
    let date = b"2026:11:23 10:20:30\0";
    // Layout: header 8, IFD0 (3 entries) 2+36+4, Exif IFD (2 entries) 2+24+4,
    // then the values.
    let ifd0_at = 8u32;
    let exif_at = ifd0_at + 2 + 36 + 4;
    let values_at = exif_at + 2 + 24 + 4;
    let make_at = values_at;
    let model_at = make_at + make.len() as u32;
    let date_at = model_at + model.len() as u32;
    let note_at = date_at + date.len() as u32;
    let mut t = b"II*\0".to_vec();
    t.extend_from_slice(&ifd0_at.to_le_bytes());
    t.extend_from_slice(&3u16.to_le_bytes());
    t.extend(entry(0x010f, 2, make.len() as u32, make_at));
    t.extend(entry(0x0110, 2, model.len() as u32, model_at));
    t.extend(entry(0x8769, 4, 1, exif_at));
    t.extend_from_slice(&0u32.to_le_bytes());
    t.extend_from_slice(&2u16.to_le_bytes());
    t.extend(entry(0x9003, 2, date.len() as u32, date_at));
    t.extend(entry(0x927c, 7, padding as u32, note_at));
    t.extend_from_slice(&0u32.to_le_bytes());
    t.extend_from_slice(make);
    t.extend_from_slice(model);
    t.extend_from_slice(date);
    t.extend(std::iter::repeat_n(0u8, padding));
    t
}

/// A HEIF file whose only item is Exif: stored in `mdat` (construction
/// method 0) or in the `meta` box's `idat` (method 1).
fn heif(exif_item: &[u8], in_idat: bool, claimed_len: Option<u32>) -> Vec<u8> {
    let ftyp = boxed(b"ftyp", b"mif1\0\0\0\0mif1heic");
    let iinf = boxed(
        b"iinf",
        &[
            &[0u8, 0, 0, 0, 0, 1][..],
            &boxed(b"infe", b"\x02\0\0\0\0\x01\0\0Exif\0"),
        ]
        .concat(),
    );
    let len = claimed_len.unwrap_or(exif_item.len() as u32);
    let iloc_body = |offset: u32| {
        let mut b = vec![1u8, 0, 0, 0, 0x44, 0, 0, 1];
        b.extend_from_slice(&1u16.to_be_bytes()); // item id
        b.extend_from_slice(&u16::from(in_idat).to_be_bytes()); // method
        b.extend_from_slice(&0u16.to_be_bytes()); // data reference
        b.extend_from_slice(&1u16.to_be_bytes()); // extents
        b.extend_from_slice(&offset.to_be_bytes());
        b.extend_from_slice(&len.to_be_bytes());
        boxed(b"iloc", &b)
    };
    let meta =
        |iloc: &[u8], idat: &[u8]| boxed(b"meta", &[&[0u8; 4][..], &iinf, iloc, idat].concat());
    if in_idat {
        let idat = boxed(b"idat", exif_item);
        [ftyp, meta(&iloc_body(0), &idat)].concat()
    } else {
        // The mdat follows meta: its payload starts after both box headers.
        let meta_len = meta(&iloc_body(0), &[]).len();
        let offset = (ftyp.len() + meta_len + 8) as u32;
        [
            ftyp,
            meta(&iloc_body(offset), &[]),
            boxed(b"mdat", exif_item),
        ]
        .concat()
    }
}

fn item(tiff: &[u8]) -> Vec<u8> {
    // Exif item: the offset (0) to the TIFF header, then the TIFF.
    [&0u32.to_be_bytes()[..], tiff].concat()
}

fn make(bytes: &[u8]) -> Option<String> {
    read_exif(bytes)?
        .get_field(exif::Tag::Make, exif::In::PRIMARY)
        .map(|f| f.display_value().to_string())
}

#[test]
fn an_exif_item_over_64_kib_is_read_in_both_storage_methods() {
    let item = item(&tiff(150_000));
    assert!(item.len() > 65_535);
    for in_idat in [false, true] {
        let bytes = heif(&item, in_idat, None);
        let exif = read_exif(&bytes).unwrap_or_else(|| panic!("idat {in_idat}"));
        let text = |tag| {
            exif.get_field(tag, exif::In::PRIMARY)
                .map(|f| f.display_value().to_string())
        };
        assert_eq!(text(exif::Tag::Make).as_deref(), Some("\"Rawtonic Test\""));
        assert_eq!(text(exif::Tag::Model).as_deref(), Some("\"Big EXIF\""));
        assert_eq!(
            text(exif::Tag::DateTimeOriginal).as_deref(),
            Some("2026-11-23 10:20:30")
        );
        // The parser kept the whole item, not a cut one.
        assert!(exif.buf().len() > 65_535, "{}", exif.buf().len());
    }
}

#[test]
fn an_item_over_the_cap_is_refused_by_the_cap() {
    // The data is really there (17 MiB, in bounds), so only the 16 MiB cap
    // can refuse it, with an exact length, with a length of zero (the rest
    // of the file or of the idat box) and in both storage methods.
    let big = item(&tiff(17 << 20));
    for in_idat in [false, true] {
        for claimed in [None, Some(0)] {
            let bytes = heif(&big, in_idat, claimed);
            assert_eq!(make(&bytes), None, "idat {in_idat}, length {claimed:?}");
        }
    }
    // Just under the cap still reads (the same builder, same paths).
    let fits = item(&tiff(15 << 20));
    for in_idat in [false, true] {
        let bytes = heif(&fits, in_idat, None);
        assert_eq!(
            make(&bytes).as_deref(),
            Some("\"Rawtonic Test\""),
            "idat {in_idat}"
        );
    }
}

#[test]
fn a_damaged_extent_length_beyond_the_file_is_refused() {
    let small = item(&tiff(10));
    for in_idat in [false, true] {
        let bytes = heif(&small, in_idat, Some(100_000_000));
        assert_eq!(make(&bytes), None, "idat {in_idat}");
    }
    assert_eq!(
        make(&heif(&small, false, None)).as_deref(),
        Some("\"Rawtonic Test\"")
    );
}
