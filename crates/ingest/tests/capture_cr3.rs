//! Synthetic CR3 metadata with a sparse image payload after its metadata.
use rawtonic_ingest::capture::{capture_facts, read_capture};
fn atom(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
    [(data.len() as u32 + 8).to_be_bytes().as_slice(), kind, data].concat()
}
fn tiff(entries: &[(u16, u16, Vec<u8>)]) -> Vec<u8> {
    let mut out = b"II\x2a\x00\x08\x00\x00\x00".to_vec();
    out.extend((entries.len() as u16).to_le_bytes());
    let mut values: Vec<u8> = Vec::new();
    for (tag, kind, value) in entries {
        out.extend(tag.to_le_bytes());
        out.extend(kind.to_le_bytes());
        let count = if *kind == 3 {
            value.len() / 2
        } else {
            value.len()
        };
        out.extend((count as u32).to_le_bytes());
        if value.len() <= 4 {
            out.extend(value);
            out.resize(out.len() + 4 - value.len(), 0);
        } else {
            out.extend((14 + entries.len() as u32 * 12 + values.len() as u32).to_le_bytes());
            values.extend(value);
        }
    }
    out.extend(0u32.to_le_bytes());
    out.extend(values);
    out
}
fn cr3() -> Vec<u8> {
    let cmt1 = tiff(&[
        (0x10f, 2, b"Canon\0".to_vec()),
        (0x110, 2, b"Canon EOS R5\0".to_vec()),
    ]);
    let cmt2 = tiff(&[
        (0x8827, 3, 5000u16.to_le_bytes().to_vec()),
        (0x9003, 2, b"2021:03:16 11:35:50\0".to_vec()),
    ]);
    let empty = tiff(&[(0, 1, vec![2, 3, 0, 0])]);
    let camera = tiff(&[(1, 3, vec![0, 0, 0, 0, 0, 0, 4, 0])]);
    let mut desc = rawler::formats::bmff::ext_cr3::cr3desc::Cr3DescBox::UUID.to_vec();
    for (kind, data) in [
        (b"CNCV", vec![0; 30]),
        (b"CCTP", vec![0; 12]),
        (b"CTBO", vec![]),
        (b"CMT1", cmt1),
        (b"CMT2", cmt2),
        (b"CMT3", camera),
        (b"CMT4", empty),
        (b"THMB", vec![]),
    ] {
        desc.extend(atom(kind, &data));
    }
    let moov = [atom(b"mvhd", &[0; 20]), atom(b"uuid", &desc)].concat();
    [
        atom(b"ftyp", b"crx \0\0\0\0crx "),
        atom(b"moov", &moov),
        [(20u32 << 20).to_be_bytes().as_slice(), b"mdat"].concat(),
    ]
    .concat()
}
#[test]
fn cr3_facts_read_metadata_without_reading_the_entire_mdat() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("photo.CR3");
    let bytes = cr3();
    std::fs::write(&path, &bytes).unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(bytes.len() as u64 - 8 + (20 << 20))
        .unwrap();
    let facts = capture_facts(&path).unwrap();
    assert_eq!(facts.captured.as_deref(), Some("2021-03-16 11:35:50"));
    assert_eq!(facts.make.as_deref(), Some("Canon"));
    assert_eq!(facts.model.as_deref(), Some("EOS R5"));
    assert_eq!(facts.iso, Some(5000));
    assert_eq!(read_capture(&path).captured.unwrap().year(), 2021);
    let lib = rawtonic_catalog::Library::create(
        &d.path().join("Library"),
        "Library",
        rawtonic_catalog::Mode::Session,
    )
    .unwrap();
    let preset = rawtonic_ingest::metadata::ImportMetadata {
        copyright: "(c) {year} Photographer".into(),
        ..Default::default()
    };
    let options = rawtonic_ingest::batch::BatchOptions {
        template: "{orig}",
        sequence_start: 1,
        backup: None,
        folder: None,
        roots: &[],
        pair_raw: None,
        album: None,
        metadata: Some(&preset),
    };
    let results = rawtonic_ingest::batch::import_batch(
        &lib,
        &[path],
        &options,
        &std::sync::atomic::AtomicBool::new(false),
        |done| {
            assert!(done.result.error.is_none(), "{:?}", done.result);
            let metadata = lib
                .metadata(done.result.asset_id.as_ref().unwrap())
                .unwrap();
            assert_eq!(metadata.copyright, "(c) 2021 Photographer");
        },
    )
    .unwrap();
    assert_eq!(results.len(), 1);
}
