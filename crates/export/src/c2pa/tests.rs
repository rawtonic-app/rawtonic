use super::*;
use ciborium::Value as C;
use ring::signature;
use serde_json::json;
use std::io::Cursor;
use x509_parser::prelude::{FromDer, X509Certificate};

fn identity() -> (tempfile::TempDir, SigningIdentity) {
    let dir = tempfile::tempdir().unwrap();
    identity::generate(dir.path(), "Test Signer").unwrap();
    let identity = identity::load(dir.path()).unwrap().unwrap();
    (dir, identity)
}

pub(super) fn plan(adjustments: &Value) -> ManifestPlan {
    plan_for(
        "/photos/DSC0001.ARW",
        adjustments,
        OutputChanges::default(),
        None,
    )
}

/// The boxes in `bytes`: (type, whole box).
fn boxes(bytes: &[u8]) -> Vec<([u8; 4], &[u8])> {
    let mut out = Vec::new();
    let mut at = 0;
    while at + 8 <= bytes.len() {
        let len = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
        out.push((
            bytes[at + 4..at + 8].try_into().unwrap(),
            &bytes[at..at + len],
        ));
        at += len;
    }
    out
}

fn label(superbox: &[u8]) -> String {
    let (_, jumd) = boxes(&superbox[8..])[0];
    let text = &jumd[8 + 17..];
    String::from_utf8(text[..text.iter().position(|b| *b == 0).unwrap()].to_vec()).unwrap()
}

/// The superbox labelled `name` anywhere below `superbox`.
fn find<'a>(superbox: &'a [u8], name: &str) -> Option<&'a [u8]> {
    if label(superbox) == name {
        return Some(superbox);
    }
    boxes(&superbox[8..])
        .into_iter()
        .filter(|(kind, _)| kind == b"jumb")
        .find_map(|(_, child)| find(child, name))
}

fn content(superbox: &[u8]) -> C {
    let (_, cbor) = boxes(&superbox[8..])[1];
    ciborium::from_reader(&cbor[8..]).unwrap()
}

/// A JSON assertion's content.
fn json_content(superbox: &[u8]) -> Value {
    let (kind, json) = boxes(&superbox[8..])[1];
    assert_eq!(&kind, b"json");
    serde_json::from_slice(&json[8..]).unwrap()
}

fn get<'a>(map: &'a C, key: &str) -> &'a C {
    map.as_map()
        .unwrap()
        .iter()
        .find(|(k, _)| k.as_text() == Some(key))
        .map(|(_, v)| v)
        .unwrap_or_else(|| panic!("no {key} in {map:?}"))
}

fn int(value: &C) -> u64 {
    u64::try_from(value.as_integer().unwrap()).unwrap()
}

/// The manifest store of a JPEG, from its APP11 segments.
fn jpeg_store(jpeg: &[u8]) -> Vec<u8> {
    let mut at = 2;
    let mut store = Vec::new();
    while jpeg[at] == 0xff && (0xe0..=0xef).contains(&jpeg[at + 1]) {
        let len = u16::from_be_bytes([jpeg[at + 2], jpeg[at + 3]]) as usize;
        if jpeg[at + 1] == 0xeb {
            let seg = &jpeg[at + 4..at + 2 + len];
            assert_eq!(&seg[..2], b"JP");
            if store.is_empty() {
                store.extend_from_slice(&seg[8..16]);
            }
            store.extend_from_slice(&seg[16..]);
        }
        at += 2 + len;
    }
    store
}

/// The manifest store of a PNG, from its `caBX` chunk.
fn png_store(png: &[u8]) -> &[u8] {
    let mut at = 8;
    loop {
        let len = u32::from_be_bytes(png[at..at + 4].try_into().unwrap()) as usize;
        if &png[at + 4..at + 8] == b"caBX" {
            let crc = u32::from_be_bytes(png[at + 8 + len..at + 12 + len].try_into().unwrap());
            assert_eq!(
                crc,
                crc32fast::hash(&png[at + 4..at + 8 + len]),
                "chunk CRC"
            );
            return &png[at + 8..at + 8 + len];
        }
        at += 12 + len;
    }
}

/// The manifest store of a WebP, from its `C2PA` chunk.
fn webp_store(webp: &[u8]) -> &[u8] {
    let mut at = 12;
    loop {
        let len = u32::from_le_bytes(webp[at + 4..at + 8].try_into().unwrap()) as usize;
        if &webp[at..at + 4] == b"C2PA" {
            return &webp[at + 8..at + 8 + len];
        }
        at += 8 + len + (len & 1);
    }
}

/// The manifest store of a JPEG XL file: its `jumb` box.
fn jxl_store(jxl: &[u8]) -> &[u8] {
    let (at, len) = bmff_boxes(jxl)
        .unwrap()
        .into_iter()
        .find(|&(at, _)| &jxl[at + 4..at + 8] == b"jumb")
        .expect("a jumb box");
    &jxl[at..at + len]
}

/// The manifest store of an AVIF, from its C2PA `uuid` box.
fn avif_store(avif: &[u8]) -> &[u8] {
    let (at, len) = bmff_boxes(avif)
        .unwrap()
        .into_iter()
        .find(|&(at, _)| &avif[at + 4..at + 8] == b"uuid" && avif[at + 8..at + 24] == BMFF_UUID)
        .expect("a C2PA uuid box");
    let whole = &avif[at..at + len];
    assert_eq!(&whole[24..28], &[0; 4], "version and flags");
    assert_eq!(&whole[28..37], b"manifest\0");
    &whole[45..]
}

/// Checks what a validator checks: the hard binding over `file`, every
/// hashed URI, and the claim signature against the signing certificate.
/// Returns the actions assertion.
fn validate(file: &[u8], store: &[u8]) -> C {
    let manifest = boxes(&store[8..])[1].1;
    if let Some(boxes_hash) = find(manifest, "c2pa.hash.boxes") {
        // Each top-level box but the manifest's own, named by its type; the
        // manifest's own box sits right before the codestream.
        let hash = content(boxes_hash);
        let listed = get(&hash, "boxes").as_array().unwrap();
        let mut at = 0;
        let mut previous = None;
        for entry in listed {
            let names = get(entry, "names").as_array().unwrap();
            assert_eq!(names.len(), 1);
            let name = names[0].as_text().unwrap();
            let (start, len) = bmff_boxes(file)
                .unwrap()
                .into_iter()
                .nth(at)
                .expect("a box for the entry");
            at += 1;
            let kind = &file[start + 4..start + 8];
            // The manifest's own box is named `C2PA`, whatever its type.
            assert_eq!(
                name.as_bytes(),
                if kind == b"jumb" { b"C2PA" } else { kind }
            );
            assert!(get(entry, "pad").as_bytes().unwrap().is_empty());
            let hash = get(entry, "hash").as_bytes().unwrap();
            if name == "C2PA" {
                assert_eq!(hash.as_slice(), &[0]);
                assert!(matches!(previous, Some(p) if p != "jxlc"));
            } else {
                assert_eq!(
                    hash.as_slice(),
                    digest::digest(&digest::SHA256, &file[start..start + len]).as_ref(),
                    "hard binding of {name}"
                );
            }
            previous = Some(name.to_string());
        }
        assert_eq!(at, bmff_boxes(file).unwrap().len(), "every box is listed");
    } else if let Some(bmff) = find(manifest, "c2pa.hash.bmff.v3") {
        // Each top-level box but the C2PA `uuid` box: its offset (64-bit
        // big-endian), then its bytes.
        let hash = content(bmff);
        let exclusions = get(&hash, "exclusions").as_array().unwrap();
        assert_eq!(exclusions.len(), 2);
        assert_eq!(get(&exclusions[0], "xpath").as_text(), Some("/uuid"));
        assert_eq!(get(&exclusions[1], "xpath").as_text(), Some("/mfra"));
        let data = &get(&exclusions[0], "data").as_array().unwrap()[0];
        assert_eq!(int(get(data, "offset")), 8);
        assert_eq!(get(data, "value").as_bytes().unwrap(), &BMFF_UUID);
        let mut bound = digest::Context::new(&digest::SHA256);
        for (at, len) in bmff_boxes(file).unwrap() {
            let whole = &file[at..at + len];
            if (&whole[4..8] == b"uuid" && whole[8..24] == BMFF_UUID) || &whole[4..8] == b"mfra" {
                continue;
            }
            bound.update(&(at as u64).to_be_bytes());
            bound.update(whole);
        }
        assert_eq!(
            get(&hash, "hash").as_bytes().unwrap().as_slice(),
            bound.finish().as_ref(),
            "hard binding"
        );
    } else {
        let hash = content(find(manifest, "c2pa.hash.data").unwrap());
        let exclusions = get(&hash, "exclusions").as_array().unwrap();
        assert_eq!(exclusions.len(), 1);
        let start = int(get(&exclusions[0], "start")) as usize;
        let len = int(get(&exclusions[0], "length")) as usize;
        let mut bound = file[..start].to_vec();
        bound.extend_from_slice(&file[start + len..]);
        assert_eq!(
            get(&hash, "hash").as_bytes().unwrap().as_slice(),
            digest::digest(&digest::SHA256, &bound).as_ref(),
            "hard binding"
        );
    }

    let claim_box = find(manifest, "c2pa.claim.v2").unwrap();
    let claim = content(claim_box);
    for uri in get(&claim, "created_assertions").as_array().unwrap() {
        let url = get(uri, "url").as_text().unwrap();
        let name = url.strip_prefix("self#jumbf=c2pa.assertions/").unwrap();
        let assertion = find(manifest, name).unwrap();
        assert_eq!(
            get(uri, "hash").as_bytes().unwrap(),
            &jumbf::superbox_hash(assertion),
            "{url}"
        );
    }

    let C::Tag(18, cose) = content(find(manifest, "c2pa.signature").unwrap()) else {
        panic!("not COSE_Sign1");
    };
    let parts = cose.as_array().unwrap();
    let protected = parts[0].as_bytes().unwrap();
    let header: C = ciborium::from_reader(protected.as_slice()).unwrap();
    let chain = header
        .as_map()
        .unwrap()
        .iter()
        .find(|(k, _)| k.as_integer() == Some(33.into()))
        .unwrap()
        .1
        .as_array()
        .unwrap();
    let cert = chain[0].as_bytes().unwrap();
    let (_, cert) = X509Certificate::from_der(cert).unwrap();
    let (_, claim_cbor) = boxes(&claim_box[8..])[1];
    let to_be_signed = Cbor::Array(vec![
        Cbor::text("Signature1"),
        Cbor::Bytes(protected.clone()),
        Cbor::Bytes(Vec::new()),
        Cbor::Bytes(claim_cbor[8..].to_vec()),
    ])
    .encode();
    signature::UnparsedPublicKey::new(
        &signature::ECDSA_P256_SHA256_FIXED,
        &cert.public_key().subject_public_key.data,
    )
    .verify(&to_be_signed, parts[3].as_bytes().unwrap())
    .expect("claim signature");
    content(find(manifest, "c2pa.actions.v2").unwrap())
}

fn action_names(actions: &C) -> Vec<String> {
    get(actions, "actions")
        .as_array()
        .unwrap()
        .iter()
        .map(|a| get(a, "action").as_text().unwrap().to_string())
        .collect()
}

pub(super) fn small_jpeg() -> Vec<u8> {
    let image =
        image::RgbImage::from_fn(32, 24, |x, y| image::Rgb([x as u8 * 8, y as u8 * 10, 90]));
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 90)
        .encode_image(&image)
        .unwrap();
    out
}

#[test]
fn plan_names_the_edits_and_each_generated_area() {
    let recipe = json!({
        "crop": {"x": 0, "y": 0, "width": 10, "height": 10},
        "orientationSteps": 1,
        "masks": [{"visible": true, "subMasks": []}],
        "aiPatches": [
            {"patchData": {"color": "A", "model": "mi-gan"}},
            {"patchData": {"color": "A"}},
            {"subMasks": [{"type": "heal"}], "patchData": {"color": "A"}}
        ]
    });
    let plan = plan_for(
        "/photos/DSC0001.ARW",
        &recipe,
        OutputChanges {
            resized: true,
            framed_or_watermarked: false,
        },
        None,
    );
    let names: Vec<_> = plan.actions.iter().map(|a| a.action).collect();
    assert_eq!(
        names,
        [
            "c2pa.opened",
            "c2pa.color_adjustments",
            "c2pa.cropped",
            "c2pa.orientation",
            "c2pa.edited",
            "c2pa.edited",
            "c2pa.edited",
            "c2pa.edited",
            "c2pa.resized"
        ]
    );
    let generated: Vec<_> = plan
        .actions
        .iter()
        .filter(|a| a.digital_source_type.is_some())
        .collect();
    assert_eq!(generated.len(), 2);
    for action in generated {
        // The same marking the file's XMP carries.
        assert_eq!(
            action.digital_source_type,
            Some(rawtonic_metadata::DIGITAL_SOURCE_COMPOSITE_AI)
        );
    }
    assert_eq!(plan.ingredient_title, "DSC0001.ARW");
    assert_eq!(plan.ingredient_format, Some("image/x-sony-arw"));
    assert_eq!(plan.author, None);

    let plain = plan_for("/p/x.jpg", &json!({}), OutputChanges::default(), Some("  "));
    let names: Vec<_> = plain.actions.iter().map(|a| a.action).collect();
    assert_eq!(names, ["c2pa.opened", "c2pa.color_adjustments"]);
    assert_eq!(plain.author, None);
}

#[test]
fn signed_jpeg_validates_and_binds_every_byte() {
    let (dir, identity) = identity();
    let jpeg = small_jpeg();
    let recipe = json!({"aiPatches": [{"patchData": {"color": "A"}}]});
    let signed = sign_jpeg(&jpeg, &plan(&recipe), &identity).unwrap();
    let store = jpeg_store(&signed);
    let actions = validate(&signed, &store);
    assert_eq!(
        action_names(&actions),
        ["c2pa.opened", "c2pa.color_adjustments", "c2pa.edited"]
    );
    let edited = &get(&actions, "actions").as_array().unwrap()[2];
    assert_eq!(
        get(edited, "digitalSourceType").as_text(),
        Some(rawtonic_metadata::DIGITAL_SOURCE_COMPOSITE_AI)
    );
    // The image itself is the encoder's, unchanged around the manifest.
    let decoded = image::load_from_memory(&signed).unwrap().to_rgb8();
    assert_eq!(decoded, image::load_from_memory(&jpeg).unwrap().to_rgb8());

    // A changed byte outside the manifest breaks the binding.
    let mut tampered = signed.clone();
    let last = tampered.len() - 3;
    tampered[last] ^= 1;
    let result = std::panic::catch_unwind(|| validate(&tampered, &store));
    assert!(result.is_err());

    // No private key in the file: the PKCS#8's ECPrivateKey scalar
    // (OCTET STRING of 32 bytes; its public key is in the certificate).
    let record: Value =
        serde_json::from_slice(&std::fs::read(dir.path().join("signing-identity.json")).unwrap())
            .unwrap();
    let key = {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD
            .decode(record["key"].as_str().unwrap())
            .unwrap()
    };
    let at = key.windows(2).position(|w| w == [0x04, 0x20]).unwrap() + 2;
    let scalar = &key[at..at + 32];
    assert!(!signed.windows(32).any(|w| w == scalar));
}

#[test]
fn the_manifest_is_bound_after_the_metadata() {
    // The export's last write before the manifest is its EXIF/XMP: a file
    // signed after it validates, and metadata written after signing breaks it.
    let (_dir, identity) = identity();
    let mut jpeg = small_jpeg();
    let mut metadata = little_exif::metadata::Metadata::new();
    metadata.set_tag(little_exif::exif_tag::ExifTag::Software(
        "Rawtonic".to_string(),
    ));
    metadata
        .write_to_vec(&mut jpeg, little_exif::filetype::FileExtension::JPEG)
        .unwrap();
    let signed = sign_jpeg(&jpeg, &plan(&json!({})), &identity).unwrap();
    validate(&signed, &jpeg_store(&signed));
    let mut later = signed.clone();
    let mut more = little_exif::metadata::Metadata::new();
    more.set_tag(little_exif::exif_tag::ExifTag::Artist(
        "Someone".to_string(),
    ));
    more.write_to_vec(&mut later, little_exif::filetype::FileExtension::JPEG)
        .unwrap();
    let store = jpeg_store(&later);
    assert!(std::panic::catch_unwind(|| validate(&later, &store)).is_err());
}

#[test]
fn signed_tiff_validates_and_keeps_its_pixels() {
    let (_dir, identity) = identity();
    let image = image::RgbImage::from_fn(40, 30, |x, y| image::Rgb([x as u8, y as u8, 7]));
    let mut file = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(image.clone())
        .write_to(&mut file, image::ImageFormat::Tiff)
        .unwrap();
    let before = file.get_ref().clone();
    sign_tiff(&mut file, &plan(&json!({"crop": {}})), &identity).unwrap();
    let signed = file.into_inner();
    assert_eq!(&signed[8..before.len()], &before[8..]);
    let decoded = image::load_from_memory(&signed).unwrap().to_rgb8();
    assert_eq!(decoded, image);

    // IFD0 points at the store with tag 0xCD41, last in the file.
    let ifd = u32::from_le_bytes(signed[4..8].try_into().unwrap()) as usize;
    let count = u16::from_le_bytes(signed[ifd..ifd + 2].try_into().unwrap()) as usize;
    let entry = (0..count)
        .map(|i| &signed[ifd + 2 + i * 12..ifd + 14 + i * 12])
        .find(|e| u16::from_le_bytes([e[0], e[1]]) == TIFF_TAG)
        .unwrap();
    let len = u32::from_le_bytes(entry[4..8].try_into().unwrap()) as usize;
    let at = u32::from_le_bytes(entry[8..12].try_into().unwrap()) as usize;
    assert_eq!(at + len, signed.len());
    let actions = validate(&signed, &signed[at..]);
    assert_eq!(
        action_names(&actions),
        ["c2pa.opened", "c2pa.color_adjustments", "c2pa.cropped"]
    );
}

#[test]
fn the_author_is_named_only_when_asked() {
    let (_dir, identity) = identity();
    let jpeg = small_jpeg();
    let without = sign_jpeg(&jpeg, &plan(&json!({})), &identity).unwrap();
    let manifest_without = jpeg_store(&without);
    assert!(find(&manifest_without, "cawg.metadata").is_none());
    let with = plan_for(
        "/p/a.dng",
        &json!({}),
        OutputChanges::default(),
        Some("Ada Example"),
    );
    let signed = sign_jpeg(&jpeg, &with, &identity).unwrap();
    let store = jpeg_store(&signed);
    validate(&signed, &store);
    let metadata = json_content(find(&store, "cawg.metadata").unwrap());
    assert_eq!(
        metadata["@context"]["dc"],
        "http://purl.org/dc/elements/1.1/"
    );
    assert_eq!(metadata["dc:creator"][0], "Ada Example");
}

#[test]
fn large_manifests_span_app11_segments() {
    assert_eq!(app11_len(8 + 10), 20 + 10);
    let n = APP11_CHUNK * 2 + 5;
    assert_eq!(app11_len(8 + n), 3 * 20 + n);
    let store = jumbf::superbox(jumbf::MANIFEST_STORE, "c2pa", &[vec![7; n as usize]]);
    let segments = app11_segments(&store);
    assert_eq!(segments.len() as u64, app11_len(store.len() as u64));
    let mut jpeg = vec![0xff, 0xd8];
    jpeg.extend_from_slice(&segments);
    jpeg.extend_from_slice(&[0xff, 0xd9]);
    assert_eq!(jpeg_store(&jpeg), store);
}

#[test]
fn only_jpeg_png_tiff_webp_avif_and_jxl_take_a_manifest() {
    for ext in ["jpg", "jpeg", "png", "tif", "tiff", "webp", "avif", "jxl"] {
        assert!(supports(ext));
    }
    for ext in ["heic", "cube"] {
        assert!(!supports(ext));
    }
}

/// A JPEG XL file as the export encodes it, with EXIF and XMP boxes.
fn small_jxl() -> Vec<u8> {
    let image = image::DynamicImage::ImageRgba8(image::RgbaImage::from_fn(40, 30, |x, y| {
        image::Rgba([x as u8 * 6, y as u8 * 8, 70, 255])
    }));
    let bare = crate::export_processing::encode_image_to_bytes(
        &image,
        "jxl",
        90,
        crate::export_processing::TiffBitDepth::Eight,
        crate::output_color_space::OutputColorSpace::Srgb,
        None,
    )
    .unwrap();
    crate::xmp_embed::jxl_container(&bare, Some(b"II*\0exif"), Some(b"<x:xmpmeta/>"))
}

#[test]
fn signed_jxl_validates_binds_every_box_and_decodes_the_same() {
    let (_dir, identity) = identity();
    let recipe = json!({"aiPatches": [{"patchData": {"color": "A"}}]});
    let plan = plan(&recipe);
    let jxl = small_jxl();
    let signed = sign_jxl(&jxl, &plan, &identity).unwrap();
    let store = jxl_store(&signed);
    let actions = validate(&signed, store);
    assert!(action_names(&actions).contains(&"c2pa.edited".to_string()));
    assert!(find(store, "c2pa.hash.data").is_none());
    // Right before the codestream, after the metadata boxes.
    let kinds: Vec<[u8; 4]> = bmff_boxes(&signed)
        .unwrap()
        .iter()
        .map(|&(at, _)| signed[at + 4..at + 8].try_into().unwrap())
        .collect();
    assert_eq!(
        kinds,
        [*b"JXL ", *b"ftyp", *b"Exif", *b"xml ", *b"jumb", *b"jxlc"]
    );
    // Every other byte is the encoder's.
    let (at, len) = bmff_boxes(&signed)
        .unwrap()
        .into_iter()
        .find(|&(at, _)| &signed[at + 4..at + 8] == b"jumb")
        .unwrap();
    let mut without = signed[..at].to_vec();
    without.extend_from_slice(&signed[at + len..]);
    assert_eq!(without, jxl);
    let decode = |bytes: &[u8]| {
        let image = jxl_oxide::JxlImage::read_with_defaults(Cursor::new(bytes)).unwrap();
        image
            .render_frame(0)
            .unwrap()
            .image_all_channels()
            .buf()
            .to_vec()
    };
    assert!(decode(&signed) == decode(&jxl));
    // A changed byte in any box, or a box moved, breaks the binding.
    for tamper in [20, signed.len() - 3] {
        let mut tampered = signed.clone();
        tampered[tamper] ^= 1;
        assert!(
            std::panic::catch_unwind(|| validate(&tampered, store)).is_err(),
            "{tamper}"
        );
    }
    let mut moved = signed[..at].to_vec();
    moved.extend_from_slice(&[0, 0, 0, 8, b'f', b'r', b'e', b'e']);
    moved.extend_from_slice(&signed[at..]);
    assert!(std::panic::catch_unwind(|| validate(&moved, store)).is_err());
    // A bare codestream (the export without metadata) is wrapped, then signed.
    let (last, _) = *bmff_boxes(&jxl).unwrap().last().unwrap();
    let bare = &jxl[last + 8..];
    assert!(bare.starts_with(&[0xff, 0x0a]));
    let wrapped = sign_jxl(bare, &plan, &identity).unwrap();
    validate(&wrapped, jxl_store(&wrapped));
    assert!(decode(&wrapped) == decode(&jxl));
    assert!(
        sign_jxl(&signed, &plan, &identity)
            .unwrap_err()
            .contains("already")
    );
    assert!(sign_jxl(b"not jxl at all", &plan, &identity).is_err());
}

/// A WebP and an AVIF as the export encodes them (with their ICC profile).
fn small_webp_and_avif() -> (Vec<u8>, Vec<u8>) {
    let image = image::DynamicImage::ImageRgba8(image::RgbaImage::from_fn(40, 30, |x, y| {
        image::Rgba([x as u8 * 6, y as u8 * 8, 70, 255])
    }));
    let encode = |format| {
        crate::export_processing::encode_image_to_bytes(
            &image,
            format,
            90,
            crate::export_processing::TiffBitDepth::Eight,
            crate::output_color_space::OutputColorSpace::Srgb,
            None,
        )
        .unwrap()
    };
    (encode("webp"), encode("avif"))
}

#[test]
fn signed_webp_and_avif_validate_and_bind_every_byte() {
    let (_dir, identity) = identity();
    let recipe = json!({"aiPatches": [{"patchData": {"color": "A"}}]});
    let plan = plan(&recipe);
    let (webp, avif) = small_webp_and_avif();

    let signed = sign_webp(&webp, &plan, &identity).unwrap();
    let store = webp_store(&signed);
    let actions = validate(&signed, store);
    assert!(action_names(&actions).contains(&"c2pa.edited".to_string()));
    let riff = u32::from_le_bytes(signed[4..8].try_into().unwrap()) as usize;
    assert_eq!(riff + 8, signed.len(), "the RIFF size counts the chunk");
    assert_eq!(
        &signed[8..webp.len()],
        &webp[8..],
        "the image chunks unchanged"
    );
    let decoded = image::load_from_memory(&signed).unwrap().to_rgb8();
    assert_eq!(decoded, image::load_from_memory(&webp).unwrap().to_rgb8());
    for at in [5, 20, webp.len() - 2] {
        let mut tampered = signed.clone();
        tampered[at] ^= 1;
        assert!(
            std::panic::catch_unwind(|| validate(&tampered, store)).is_err(),
            "{at}"
        );
    }
    assert!(
        sign_webp(&signed, &plan, &identity)
            .unwrap_err()
            .contains("already")
    );

    let signed = sign_avif(&avif, &plan, &identity).unwrap();
    let store = avif_store(&signed);
    let actions = validate(&signed, store);
    assert!(action_names(&actions).contains(&"c2pa.edited".to_string()));
    assert!(find(store, "c2pa.hash.data").is_none());
    // The manifest right before the media data, the items moved with it:
    // the same pixels and ICC profile as the encoder's file.
    let kinds: Vec<[u8; 4]> = bmff_boxes(&signed)
        .unwrap()
        .iter()
        .map(|&(at, _)| signed[at + 4..at + 8].try_into().unwrap())
        .collect();
    let mdat = kinds.iter().position(|k| k == b"mdat").unwrap();
    assert_eq!(&kinds[mdat - 1], b"uuid", "{kinds:?}");
    let decoded = rawtonic_engine::avif::decode(&signed).unwrap();
    let original = rawtonic_engine::avif::decode(&avif).unwrap();
    assert_eq!(
        image::GenericImageView::dimensions(&decoded.image),
        (40, 30)
    );
    assert!(decoded.image.to_rgb32f() == original.image.to_rgb32f());
    assert_eq!(decoded.icc, original.icc);
    // A changed byte, or a box moved (here a `free` box put in front of the
    // last one: every later offset changes), breaks the binding.
    let mut tampered = signed.clone();
    tampered[signed.len() - 3] ^= 1;
    assert!(std::panic::catch_unwind(|| validate(&tampered, store)).is_err());
    let boxes = bmff_boxes(&signed).unwrap();
    let (last, _) = boxes[boxes.len() - 1];
    let mut moved = signed[..last].to_vec();
    moved.extend_from_slice(&[0, 0, 0, 8, b'f', b'r', b'e', b'e']);
    moved.extend_from_slice(&signed[last..]);
    assert!(std::panic::catch_unwind(|| validate(&moved, store)).is_err());
    assert!(
        sign_avif(&signed, &plan, &identity)
            .unwrap_err()
            .contains("already")
    );

    // Files the signers cannot take whole: refused, not half signed.
    let mut open_ended = avif.clone();
    open_ended.extend_from_slice(&[0, 0, 0, 0, b'm', b'd', b'a', b't', 1, 2]);
    assert!(
        sign_avif(&open_ended, &plan, &identity)
            .unwrap_err()
            .contains("runs to its end")
    );
    let mut simple = Vec::new();
    simple.extend_from_slice(b"RIFF");
    simple.extend_from_slice(&12u32.to_le_bytes());
    simple.extend_from_slice(b"WEBPVP8L");
    simple.extend_from_slice(&0u32.to_le_bytes());
    assert!(
        sign_webp(&simple, &plan, &identity)
            .unwrap_err()
            .contains("extended")
    );
}

/// Keeps a signed sample in the folder `C2PA_KEEP` names, never replacing
/// a file there (no-clobber publish: an existing name fails the test).
fn keep_sample(folder: &str, name: &str, bytes: &[u8]) {
    let folder = std::path::Path::new(folder);
    let mut temp = tempfile::NamedTempFile::new_in(folder).unwrap();
    temp.write_all(bytes).unwrap();
    temp.as_file().sync_all().unwrap();
    rawtonic_fs::no_clobber::persist(temp, &folder.join(name))
        .unwrap_or_else(|e| panic!("{name} not kept in {}: {e}", folder.display()));
}

/// The C2PA reference validator on our files: `C2PATOOL=/path/to/c2patool
/// cargo test -p rawtonic-export --lib c2pa::tests::c2patool -- --ignored`
/// (c2patool is a test tool only, never a dependency). Every file must be
/// valid, its only finding the unknown signer of a local certificate, with
/// the actions and AI marking we wrote.
#[test]
#[ignore = "needs c2patool (C2PATOOL=path)"]
fn c2patool_validates_signed_files() {
    let tool = std::env::var("C2PATOOL").expect("C2PATOOL");
    let (_dir, identity) = identity();
    let out = tempfile::tempdir().unwrap();
    let recipe = json!({
        "crop": {},
        "aiPatches": [{"patchData": {"color": "A", "model": "moebius"}}]
    });
    let plan = plan_for(
        "/photos/DSC0001.ARW",
        &recipe,
        OutputChanges {
            resized: true,
            framed_or_watermarked: true,
        },
        Some("Ada Example"),
    );
    let jpeg = sign_jpeg(&small_jpeg(), &plan, &identity).unwrap();
    let image = image::RgbImage::from_fn(40, 30, |x, y| image::Rgb([x as u8, y as u8, 7]));
    let mut tiff = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(image)
        .write_to(&mut tiff, image::ImageFormat::Tiff)
        .unwrap();
    sign_tiff(&mut tiff, &plan, &identity).unwrap();
    let mut png = Vec::new();
    image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(40, 30, |x, y| {
        image::Rgb([x as u8, y as u8, 7])
    }))
    .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
    .unwrap();
    let png = sign_png(&png, &plan, &identity).unwrap();
    let (webp, avif) = small_webp_and_avif();
    let webp = sign_webp(&webp, &plan, &identity).unwrap();
    let avif = sign_avif(&avif, &plan, &identity).unwrap();
    let jxl = sign_jxl(&small_jxl(), &plan, &identity).unwrap();
    for (name, bytes) in [
        ("signed.jpg", jpeg),
        ("signed.png", png),
        ("signed.tif", tiff.into_inner()),
        ("signed.webp", webp),
        ("signed.avif", avif),
        ("signed.jxl", jxl),
    ] {
        let path = out.path().join(name);
        std::fs::write(&path, &bytes).unwrap();
        if let Ok(keep) = std::env::var("C2PA_KEEP") {
            keep_sample(&keep, name, &bytes);
        }
        let report = std::process::Command::new(&tool)
            .arg(&path)
            .output()
            .unwrap();
        let report: Value = serde_json::from_slice(&report.stdout)
            .unwrap_or_else(|e| panic!("{name}: {e}: {}", String::from_utf8_lossy(&report.stdout)));
        assert_eq!(report["validation_state"], "Valid", "{name}: {report:#}");
        let codes: Vec<_> = report["validation_status"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["code"].as_str().unwrap())
            .collect();
        assert_eq!(codes, ["signingCredential.untrusted"], "{name}: {report:#}");
        let manifest = &report["manifests"][report["active_manifest"].as_str().unwrap()];
        assert_eq!(manifest["claim_generator_info"][0]["name"], "Rawtonic");
        assert_eq!(manifest["signature_info"]["common_name"], "Test Signer");
        let actions = manifest["assertions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["label"] == "c2pa.actions.v2")
            .unwrap();
        let actions = actions["data"]["actions"].as_array().unwrap();
        let names: Vec<_> = actions
            .iter()
            .map(|a| a["action"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "c2pa.opened",
                "c2pa.color_adjustments",
                "c2pa.cropped",
                "c2pa.edited",
                "c2pa.resized",
                "c2pa.edited"
            ],
            "{name}"
        );
        assert_eq!(
            actions[3]["digitalSourceType"],
            rawtonic_metadata::DIGITAL_SOURCE_COMPOSITE_AI,
            "{name}"
        );
        // The author as the reference SDK reads it (a metadata assertion it
        // validates: no `assertion.*` failure above).
        let metadata = manifest["assertions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["label"] == "cawg.metadata")
            .unwrap_or_else(|| panic!("{name}: no cawg.metadata: {manifest:#}"));
        assert_eq!(metadata["data"]["dc:creator"][0], "Ada Example", "{name}");
        assert_eq!(manifest["ingredients"][0]["title"], "DSC0001.ARW");
        assert_eq!(manifest["ingredients"][0]["relationship"], "parentOf");
        assert!(manifest.get("thumbnail").is_none(), "{name}: no thumbnail");
    }
}

/// A user's own certificate (RSA or ECDSA PKCS#12 made with OpenSSL):
/// `C2PA_P12=file C2PA_P12_PASSWORD=… C2PATOOL=… cargo test -p
/// rawtonic-export --lib c2pa::tests::imported -- --ignored`.
#[test]
#[ignore = "needs a PKCS#12 file and c2patool (C2PA_P12, C2PA_P12_PASSWORD, C2PATOOL)"]
fn imported_certificate_signs_files_c2patool_accepts() {
    let tool = std::env::var("C2PATOOL").expect("C2PATOOL");
    let p12 = std::env::var("C2PA_P12").expect("C2PA_P12");
    let password = std::env::var("C2PA_P12_PASSWORD").unwrap_or_default();
    let dir = tempfile::tempdir().unwrap();
    let info = identity::import_pkcs12(dir.path(), std::path::Path::new(&p12), &password).unwrap();
    eprintln!("imported: {info:?}");
    let identity = identity::load(dir.path()).unwrap().unwrap();
    let signed = sign_jpeg(&small_jpeg(), &plan(&json!({})), &identity).unwrap();
    if info.algorithm == "ES256" {
        validate(&signed, &jpeg_store(&signed));
    }
    let path = dir.path().join("signed.jpg");
    std::fs::write(&path, &signed).unwrap();
    if let Ok(keep) = std::env::var("C2PA_KEEP") {
        keep_sample(&keep, "imported.jpg", &signed);
    }
    let report = std::process::Command::new(&tool)
        .arg(&path)
        .output()
        .unwrap();
    let report: Value = serde_json::from_slice(&report.stdout).unwrap();
    assert_eq!(report["validation_state"], "Valid", "{report:#}");
    let codes: Vec<_> = report["validation_status"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["code"].as_str().unwrap())
        .collect();
    assert_eq!(codes, ["signingCredential.untrusted"], "{report:#}");
}

mod export {
    //! The export path (`save_rendered_export`) with and without the option.
    use super::*;
    use crate::export_processing::{
        EncodedExport, ExportOutput, ExportSettings, PROVENANCE_NEEDS_IDENTITY,
        save_rendered_export,
    };
    use std::cell::RefCell;
    use std::path::Path;

    fn settings(provenance: bool) -> ExportSettings {
        let mut value = json!({
            "jpegQuality": 90, "resize": null, "keepMetadata": true, "colorSpace": "srgb",
            "stripGps": false, "filenameTemplate": null, "watermark": null,
        });
        if provenance {
            value["provenance"] = json!({"includeAuthor": true});
        }
        serde_json::from_value(value).unwrap()
    }

    fn render() -> image::DynamicImage {
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_fn(48, 32, |x, y| {
            image::Rgba([x as u8 * 5, y as u8 * 7, 60, 255])
        }))
    }

    /// The file `save_rendered_export` writes for `extension`, or its error
    /// (and whether anything was written).
    fn export(
        extension: &str,
        settings: &ExportSettings,
        identity: Option<&SigningIdentity>,
        recipe: &Value,
    ) -> (Result<(), String>, Option<Vec<u8>>) {
        // `WriteExport` is a 'static closure type.
        let written = std::rc::Rc::new(RefCell::new(None));
        let into = written.clone();
        let write = move |_: &Path, _: &str, encoded: EncodedExport<'_>| {
            *into.borrow_mut() = Some(encoded.into_bytes()?);
            Ok(())
        };
        let catalog = || {
            Some(rawtonic_metadata::Metadata {
                creator: "Ada Example".to_string(),
                ..Default::default()
            })
        };
        let path = format!("/out/photo.{extension}");
        let result = save_rendered_export(
            render(),
            "/photos/DSC0001.ARW",
            recipe,
            settings,
            &ExportOutput {
                path: Path::new(&path),
                format: extension,
                exif_sidecar: &|| None,
                catalog: &catalog,
                enlarger: None,
                write: &write,
                provenance: identity,
            },
            &|| Ok(()),
        );
        (result, written.take())
    }

    fn tiff_store(tiff: &[u8]) -> &[u8] {
        let ifd = u32::from_le_bytes(tiff[4..8].try_into().unwrap()) as usize;
        let count = u16::from_le_bytes(tiff[ifd..ifd + 2].try_into().unwrap()) as usize;
        let entry = (0..count)
            .map(|i| &tiff[ifd + 2 + i * 12..ifd + 14 + i * 12])
            .find(|e| u16::from_le_bytes([e[0], e[1]]) == TIFF_TAG)
            .expect("a C2PA tag");
        let len = u32::from_le_bytes(entry[4..8].try_into().unwrap()) as usize;
        let at = u32::from_le_bytes(entry[8..12].try_into().unwrap()) as usize;
        &tiff[at..at + len]
    }

    #[test]
    fn signed_exports_bind_their_metadata_and_ai_marking() {
        let (_dir, identity) = identity();
        let recipe = json!({"aiPatches": [{"patchData": {"color": "A"}}]});
        for extension in ["jpg", "png", "tif", "webp", "avif", "jxl"] {
            let (result, file) = export(extension, &settings(true), Some(&identity), &recipe);
            result.unwrap();
            let file = file.unwrap();
            if let Ok(keep) = std::env::var("C2PA_KEEP") {
                super::keep_sample(&keep, &format!("export.{extension}"), &file);
            }
            let store = match extension {
                "jpg" => jpeg_store(&file),
                "png" => png_store(&file).to_vec(),
                "webp" => webp_store(&file).to_vec(),
                "avif" => avif_store(&file).to_vec(),
                "jxl" => jxl_store(&file).to_vec(),
                _ => tiff_store(&file).to_vec(),
            };
            let actions = validate(&file, &store);
            assert!(action_names(&actions).contains(&"c2pa.edited".to_string()));
            // The XMP marking is in the bound bytes: written before the manifest.
            let marking = rawtonic_metadata::DIGITAL_SOURCE_COMPOSITE_AI.as_bytes();
            let at = file.windows(marking.len()).position(|w| w == marking);
            assert!(at.is_some(), "{extension}: the XMP marking");
            let metadata = json_content(find(&store, "cawg.metadata").unwrap());
            assert_eq!(metadata["dc:creator"][0], "Ada Example");
        }
    }

    #[test]
    fn without_the_option_nothing_changes() {
        let (_dir, identity) = identity();
        for extension in ["jpg", "tif", "png", "webp", "avif", "jxl"] {
            let (result, plain) = export(extension, &settings(false), Some(&identity), &json!({}));
            result.unwrap();
            let plain = plain.unwrap();
            let (result, without_identity) = export(extension, &settings(false), None, &json!({}));
            result.unwrap();
            assert_eq!(plain, without_identity.unwrap(), "{extension}");
            assert!(!plain.windows(4).any(|w| w == b"jumb"), "{extension}");
        }
        // The setting is absent from the recipe unless it is on.
        let saved = serde_json::to_value(settings(false)).unwrap();
        assert!(saved.get("provenance").is_none(), "{saved}");
        let saved = serde_json::to_value(settings(true)).unwrap();
        assert_eq!(saved["provenance"], json!({"includeAuthor": true}));
    }

    #[test]
    fn the_manifest_describes_the_main_file_and_finds_the_embedded_author() {
        let (_dir, identity) = identity();
        let recipe = json!({"masks": [{"visible": true, "subMasks": []}]});
        let written = std::rc::Rc::new(RefCell::new(None));
        let into = written.clone();
        let write = move |_: &Path, _: &str, encoded: EncodedExport<'_>| {
            *into.borrow_mut() = Some(encoded.into_bytes()?);
            Ok(())
        };
        // A photo outside any library: the author is its EXIF Artist.
        let exif = || {
            Some(
                [("Artist".to_string(), "Exif Person".to_string())]
                    .into_iter()
                    .collect(),
            )
        };
        for export_masks in [false, true] {
            let mut settings = settings(true);
            settings.export_masks = export_masks;
            save_rendered_export(
                render(),
                "/photos/DSC0001.ARW",
                &recipe,
                &settings,
                &ExportOutput {
                    path: Path::new("/out/photo.jpg"),
                    format: "jpg",
                    exif_sidecar: &exif,
                    catalog: &|| None,
                    enlarger: None,
                    write: &write,
                    provenance: Some(&identity),
                },
                &|| Ok(()),
            )
            .unwrap();
            let file = written.take().unwrap();
            let store = jpeg_store(&file);
            let actions = action_names(&validate(&file, &store));
            // With mask files the main file is rendered without its layers.
            assert_eq!(
                actions.contains(&"c2pa.edited".to_string()),
                !export_masks,
                "{actions:?}"
            );
            let metadata = json_content(find(&store, "cawg.metadata").unwrap());
            assert_eq!(metadata["dc:creator"][0], "Exif Person");
        }
    }

    #[test]
    fn a_queued_export_signs_only_with_its_identity() {
        let (_dir, identity) = identity();
        let mut settings = settings(true);
        settings.provenance.as_mut().unwrap().signer = Some("another".to_string());
        let (result, file) = export("jpg", &settings, Some(&identity), &json!({}));
        assert_eq!(
            result.unwrap_err(),
            crate::export_processing::PROVENANCE_SIGNER_CHANGED
        );
        assert!(file.is_none());
        settings.provenance.as_mut().unwrap().signer = Some(identity.fingerprint());
        let (result, _) = export("jpg", &settings, Some(&identity), &json!({}));
        result.unwrap();
    }

    #[test]
    fn an_export_that_cannot_be_signed_is_not_written() {
        let (_dir, identity) = identity();
        let (result, file) = export("jpg", &settings(true), None, &json!({}));
        assert_eq!(result.unwrap_err(), PROVENANCE_NEEDS_IDENTITY);
        assert!(file.is_none());
        // A format with no manifest binding.
        assert!(
            crate::export_processing::provenance_format_error("heic")
                .unwrap()
                .contains("cannot be written to")
        );
        assert!(crate::export_processing::provenance_format_error("jxl").is_none());
    }
}

/// Signing time of a 24 MP export (6000 × 4000), the ADOPT2 bar (≤ 50 ms):
/// `C2PA_TIME_DIR=<dir on the export disk> cargo test --release -p
/// rawtonic-export --lib c2pa::tests::overhead -- --ignored --nocapture`.
/// JPEG: in memory, as the export signs it. TIFF: a file on disk read back
/// once for its hash, as a streamed export's is.
#[test]
#[ignore = "timing (release build, quiet host)"]
fn overhead_per_24mp_export() {
    use std::time::Instant;
    let (_dir, identity) = identity();
    let plan = plan_for(
        "/photos/DSC0001.ARW",
        &json!({"crop": {}, "aiPatches": [{"patchData": {"color": "A"}}]}),
        OutputChanges::default(),
        Some("Ada Example"),
    );
    let (w, h) = (6000u32, 4000u32);
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    let mut noise = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed & 0x3f) as u16
    };
    let image = image::ImageBuffer::<image::Rgb<u16>, _>::from_fn(w, h, |x, y| {
        let base = ((x * 40 + y * 25) % 50_000) as u16;
        image::Rgb([
            base + noise() * 64,
            base / 2 + noise() * 64,
            20_000 + noise() * 64,
        ])
    });
    let image = image::DynamicImage::ImageRgb16(image);
    let rgb8 = image.to_rgb8();
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 90)
        .encode_image(&rgb8)
        .unwrap();
    let median = |mut v: Vec<f64>| {
        v.sort_by(f64::total_cmp);
        (v[v.len() / 2], v[0], v[v.len() - 1])
    };
    let mut times = Vec::new();
    for _ in 0..9 {
        let start = Instant::now();
        let signed = sign_jpeg(&jpeg, &plan, &identity).unwrap();
        times.push(start.elapsed().as_secs_f64() * 1e3);
        assert!(signed.len() > jpeg.len());
    }
    let (med, min, max) = median(times);
    eprintln!(
        "JPEG {:.1} MB: sign median {med:.2} ms (min {min:.2}, max {max:.2})",
        jpeg.len() as f64 / 1e6
    );

    for (name, png_image) in [
        ("8-bit", image::DynamicImage::ImageRgb8(rgb8.clone())),
        ("16-bit", image.clone()),
    ] {
        let mut png = Vec::new();
        png_image
            .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let mut times = Vec::new();
        for _ in 0..7 {
            let start = Instant::now();
            sign_png(&png, &plan, &identity).unwrap();
            times.push(start.elapsed().as_secs_f64() * 1e3);
        }
        let (med, min, max) = median(times);
        eprintln!(
            "PNG {name} {:.0} MB: sign median {med:.1} ms (min {min:.1}, max {max:.1})",
            png.len() as f64 / 1e6
        );
    }

    // Real exports too (`C2PA_TIME_FILES`: JPEG paths, comma-separated).
    for path in std::env::var("C2PA_TIME_FILES")
        .unwrap_or_default()
        .split(',')
        .filter(|p| !p.is_empty())
    {
        let jpeg = std::fs::read(path).unwrap();
        let mut times = Vec::new();
        for _ in 0..9 {
            let start = Instant::now();
            sign_jpeg(&jpeg, &plan, &identity).unwrap();
            times.push(start.elapsed().as_secs_f64() * 1e3);
        }
        let (med, min, max) = median(times);
        eprintln!(
            "{path} ({:.1} MB): sign median {med:.2} ms (min {min:.2}, max {max:.2})",
            jpeg.len() as f64 / 1e6
        );
    }

    let dir = std::env::var("C2PA_TIME_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir());
    for (name, tiff) in [
        ("8-bit", image::DynamicImage::ImageRgb8(rgb8.clone())),
        ("16-bit", image.clone()),
    ] {
        let mut encoded = Cursor::new(Vec::new());
        tiff.write_to(&mut encoded, image::ImageFormat::Tiff)
            .unwrap();
        let encoded = encoded.into_inner();
        let mut times = Vec::new();
        for i in 0..7 {
            let path = dir.join(format!("c2pa-timing-{name}-{i}.tif"));
            std::fs::write(&path, &encoded).unwrap();
            let mut file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
                .unwrap();
            let start = Instant::now();
            sign_tiff(&mut file, &plan, &identity).unwrap();
            // The export's own fsync of the file follows either way.
            times.push(start.elapsed().as_secs_f64() * 1e3);
            drop(file);
            std::fs::remove_file(&path).unwrap();
        }
        let (med, min, max) = median(times);
        eprintln!(
            "TIFF {name} {:.0} MB: sign median {med:.1} ms (min {min:.1}, max {max:.1})",
            encoded.len() as f64 / 1e6
        );
    }
}
