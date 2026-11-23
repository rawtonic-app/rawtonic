//! Provenance manifests (C2PA 2.x) signed into exported files, when an
//! export recipe asks for them (`ExportSettings::provenance`; off by
//! default, so other exports are unchanged).
//!
//! Rawtonic writes the manifest itself (user decision 2026-11-21, notes
//! `benchmark-results/c2p1-2026-11-21/REPORT.md`): JUMBF, CBOR and a
//! COSE_Sign1 signature made with `ring`, no network access (no time-stamp
//! authority, no remote manifests). A manifest holds one claim with:
//!
//! - an ingredient for the photograph the export was developed from
//!   (`parentOf`; its file name and type, no thumbnail),
//! - the actions (`c2pa.opened`, `c2pa.color_adjustments`, and as the
//!   recipe has them `c2pa.cropped`, `c2pa.orientation`, `c2pa.edited`,
//!   `c2pa.resized`), and one `c2pa.edited` per area a generative model
//!   made, with the IPTC Digital Source Type the file's XMP carries
//!   (`DIGITAL_SOURCE_COMPOSITE_AI`),
//! - the author (`cawg.metadata`, `dc:creator`), only when the recipe asks,
//! - the hard binding: SHA-256 of the whole file except the manifest,
//!   computed after every other byte of the file is written
//!   (`c2pa.hash.data`; for AVIF `c2pa.hash.bmff.v3`, which also binds each
//!   top-level box's position).
//!
//! JPEG (APP11 segments), PNG (a `caBX` chunk), TIFF (tag 0xCD41), WebP (a
//! `C2PA` chunk) and AVIF (a C2PA `uuid` box before the media data) are written; other formats
//! refuse the option ([`supports`]).

pub mod cbor;
mod der;
pub mod identity;
mod jumbf;

use std::io::{Read, Seek, SeekFrom, Write};

use ring::digest;
use serde::Serialize;
use serde_json::Value;

use cbor::Cbor;
pub use identity::{IdentityInfo, IdentityKind, SigningIdentity};

/// The name a manifest gives as its claim generator.
pub const GENERATOR: &str = "Rawtonic";

/// The TIFF tag a C2PA manifest store is kept in.
pub const TIFF_TAG: u16 = 0xCD41;

/// Whether files of `extension` (lower case) can carry a manifest.
pub fn supports(extension: &str) -> bool {
    matches!(
        extension,
        "jpg" | "jpeg" | "png" | "tif" | "tiff" | "webp" | "avif"
    )
}

/// The extended type of the `uuid` box a BMFF file keeps its manifest store
/// in (C2PA 2.x, "Embedding manifests into BMFF-based assets").
const BMFF_UUID: [u8; 16] = [
    0xd8, 0xfe, 0xc3, 0xd6, 0x1b, 0x0e, 0x48, 0x3c, 0x92, 0x97, 0x58, 0x28, 0x87, 0x7e, 0xc4, 0x81,
];

/// What the manifest's hard binding says.
#[derive(Clone, Copy)]
enum Binding<'b> {
    /// `c2pa.hash.data`: SHA-256 of the file without the bytes `exclusion`
    /// (start, length) that hold the manifest.
    Data {
        exclusion: (u64, u64),
        hash: &'b [u8],
    },
    /// `c2pa.hash.bmff.v3`: SHA-256 of each top-level box's offset and bytes
    /// but the manifest's `uuid` box and any `mfra`.
    Bmff { hash: &'b [u8] },
}

/// One action of the manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Action {
    /// A C2PA action name (`c2pa.opened`, …).
    pub action: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub digital_source_type: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl Action {
    fn new(action: &'static str, description: &str) -> Self {
        Self {
            action,
            digital_source_type: None,
            description: Some(description.to_string()),
        }
    }
}

/// Everything a photo's manifest says, before it is signed: also what the
/// Export dialog shows (serialized as is).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestPlan {
    pub generator: &'static str,
    pub generator_version: &'static str,
    /// The file name of the photograph developed (the `parentOf` ingredient).
    pub ingredient_title: String,
    /// Its media type, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ingredient_format: Option<&'static str>,
    pub actions: Vec<Action>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
}

/// What an export changed besides the recipe.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OutputChanges {
    /// Its pixel size differs from the developed photograph's (resize,
    /// Super Resolution).
    pub resized: bool,
    /// A border, padding or watermark was added.
    pub framed_or_watermarked: bool,
}

/// The manifest plan of an export of `source_path` developed with
/// `adjustments`. `author`: the creator to name, when the recipe asks.
pub fn plan_for(
    source_path: &str,
    adjustments: &Value,
    changes: OutputChanges,
    author: Option<&str>,
) -> ManifestPlan {
    use rawtonic_recipe::geometry;
    use rawtonic_recipe::masks;
    let path = std::path::Path::new(source_path);
    let ingredient_title = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ingredient_format = path
        .extension()
        .and_then(|e| e.to_str())
        .and_then(|e| media_type(&e.to_ascii_lowercase()));

    let mut actions = vec![
        Action::new("c2pa.opened", "Opened the photograph"),
        Action::new("c2pa.color_adjustments", "Developed: tone and colour"),
    ];
    let number = |key: &str| adjustments.get(key).and_then(Value::as_f64).unwrap_or(0.0);
    let flag = |key: &str| adjustments.get(key).and_then(Value::as_bool) == Some(true);
    if adjustments
        .get(geometry::CROP.name())
        .is_some_and(|crop| !crop.is_null())
    {
        actions.push(Action::new("c2pa.cropped", "Cropped"));
    }
    if number(geometry::ORIENTATION_STEPS.name()) != 0.0
        || number(geometry::ROTATION.name()) != 0.0
        || flag(geometry::FLIP_HORIZONTAL.name())
        || flag(geometry::FLIP_VERTICAL.name())
    {
        actions.push(Action::new(
            "c2pa.orientation",
            "Rotated, straightened or flipped",
        ));
    }
    let local_adjustments = adjustments
        .get(masks::MASKS.name())
        .and_then(Value::as_array)
        .is_some_and(|layers| {
            layers.iter().any(|layer| {
                layer.get(masks::VISIBLE.name()).and_then(Value::as_bool) != Some(false)
            })
        });
    if local_adjustments {
        actions.push(Action::new("c2pa.edited", "Local adjustments"));
    }
    if rawtonic_metadata::provenance::has_hand_made_patches(adjustments) {
        actions.push(Action::new(
            "c2pa.edited",
            "Retouched (clone, heal, liquify or retouch)",
        ));
    }
    for model in rawtonic_metadata::provenance::generated_fills(adjustments) {
        let model = match model {
            Some(rawtonic_metadata::provenance::fill_model::MOEBIUS) => " (Moebius)",
            Some(rawtonic_metadata::provenance::fill_model::MI_GAN) => " (MI-GAN)",
            Some(rawtonic_metadata::provenance::fill_model::EXTERNAL) => " (external service)",
            _ => "",
        };
        actions.push(Action {
            action: "c2pa.edited",
            digital_source_type: Some(rawtonic_metadata::DIGITAL_SOURCE_COMPOSITE_AI),
            description: Some(format!("Area filled by a generative model{model}")),
        });
    }
    if changes.resized {
        actions.push(Action::new("c2pa.resized", "Resized for export"));
    }
    if changes.framed_or_watermarked {
        actions.push(Action::new(
            "c2pa.edited",
            "Border, padding or watermark added",
        ));
    }
    ManifestPlan {
        generator: GENERATOR,
        generator_version: env!("CARGO_PKG_VERSION"),
        ingredient_title,
        ingredient_format,
        actions,
        author: author
            .map(str::trim)
            .filter(|a| !a.is_empty())
            .map(str::to_string),
    }
}

/// The media type of a source file's extension (RAW types as the
/// `image/x-…` names verifiers know).
fn media_type(extension: &str) -> Option<&'static str> {
    Some(match extension {
        "jpg" | "jpeg" => "image/jpeg",
        "tif" | "tiff" => "image/tiff",
        "png" => "image/png",
        "heic" | "heif" | "hif" => "image/heic",
        "avif" => "image/avif",
        "webp" => "image/webp",
        "jxl" => "image/jxl",
        "dng" => "image/x-adobe-dng",
        "arw" | "srf" | "sr2" => "image/x-sony-arw",
        "cr2" => "image/x-canon-cr2",
        "cr3" => "image/x-canon-cr3",
        "crw" => "image/x-canon-crw",
        "nef" | "nrw" => "image/x-nikon-nef",
        "raf" => "image/x-fuji-raf",
        "orf" => "image/x-olympus-orf",
        "rw2" => "image/x-panasonic-rw2",
        "pef" => "image/x-pentax-pef",
        "srw" => "image/x-samsung-srw",
        "3fr" | "fff" => "image/x-hasselblad-3fr",
        "iiq" => "image/x-phaseone-iiq",
        "x3f" => "image/x-sigma-x3f",
        "rwl" => "image/x-leica-rwl",
        _ => return None,
    })
}

/// One manifest being made: its labels stay the same between the sizing
/// passes and the signed one.
struct Manifest<'a> {
    plan: &'a ManifestPlan,
    identity: &'a SigningIdentity,
    label: String,
    instance_id: String,
}

impl<'a> Manifest<'a> {
    fn new(plan: &'a ManifestPlan, identity: &'a SigningIdentity) -> Self {
        Self {
            plan,
            identity,
            label: format!("urn:c2pa:{}", uuid::Uuid::new_v4()),
            instance_id: format!("xmp:iid:{}", uuid::Uuid::new_v4()),
        }
    }

    /// The manifest store for a file `binding` describes. Unsigned (a zero
    /// signature of the right length) to find its size.
    fn store(&self, binding: Binding<'_>, signed: bool) -> Result<Vec<u8>, String> {
        if signed {
            self.identity.check_usable()?;
        }
        let plan = self.plan;
        let mut assertions: Vec<(&str, Vec<u8>)> = Vec::new();

        let mut ingredient = vec![
            ("relationship", Cbor::text("parentOf")),
            ("dc:title", Cbor::text(&plan.ingredient_title)),
        ];
        if let Some(format) = plan.ingredient_format {
            ingredient.push(("dc:format", Cbor::text(format)));
        }
        let ingredient_label = "c2pa.ingredient.v3";
        let ingredient_box = jumbf::cbor_superbox(
            jumbf::CBOR,
            ingredient_label,
            &Cbor::map(ingredient).encode(),
        );
        let ingredient_ref = hashed_uri(ingredient_label, &ingredient_box);
        assertions.push((ingredient_label, ingredient_box));

        let actions = plan
            .actions
            .iter()
            .map(|action| {
                let mut entries = vec![("action", Cbor::text(action.action))];
                if action.action == "c2pa.opened" {
                    entries.push((
                        "parameters",
                        Cbor::map([("ingredients", Cbor::Array(vec![ingredient_ref.clone()]))]),
                    ));
                }
                if let Some(source) = action.digital_source_type {
                    entries.push(("digitalSourceType", Cbor::text(source)));
                }
                if let Some(description) = &action.description {
                    entries.push(("description", Cbor::text(description)));
                }
                Cbor::map(entries)
            })
            .collect();
        let actions_label = "c2pa.actions.v2";
        assertions.push((
            actions_label,
            jumbf::cbor_superbox(
                jumbf::CBOR,
                actions_label,
                &Cbor::map([("actions", Cbor::Array(actions))]).encode(),
            ),
        ));

        if let Some(author) = &plan.author {
            // The creator in a CAWG metadata assertion (JSON-LD, Dublin
            // Core): C2PA 2.x moved human-made metadata out of claims'
            // CreativeWork into `*.metadata` assertions, and `dc:creator` is
            // not among `c2pa.metadata`'s fields.
            let label = "cawg.metadata";
            let metadata = serde_json::json!({
                "@context": {"dc": "http://purl.org/dc/elements/1.1/"},
                "dc:creator": [author],
            });
            let json = serde_json::to_vec(&metadata).expect("a JSON-LD assertion");
            assertions.push((label, jumbf::json_superbox(label, &json)));
        }

        let (hash_label, binding) = match binding {
            Binding::Data { exclusion, hash } => (
                "c2pa.hash.data",
                Cbor::map([
                    (
                        "exclusions",
                        Cbor::Array(vec![Cbor::map([
                            ("start", int(exclusion.0)?),
                            ("length", int(exclusion.1)?),
                        ])]),
                    ),
                    ("name", Cbor::text("jumbf manifest")),
                    ("alg", Cbor::text("sha256")),
                    ("hash", Cbor::Bytes(hash.to_vec())),
                    ("pad", Cbor::Bytes(Vec::new())),
                ]),
            ),
            Binding::Bmff { hash } => (
                "c2pa.hash.bmff.v3",
                Cbor::map([
                    (
                        "exclusions",
                        // The C2PA `uuid` box, and `/mfra`, which every
                        // BMFF binding lists (C2PA 2.x, A.5.5).
                        Cbor::Array(vec![
                            Cbor::map([
                                ("xpath", Cbor::text("/uuid")),
                                (
                                    "data",
                                    Cbor::Array(vec![Cbor::map([
                                        ("offset", Cbor::Int(8)),
                                        ("value", Cbor::Bytes(BMFF_UUID.to_vec())),
                                    ])]),
                                ),
                            ]),
                            Cbor::map([("xpath", Cbor::text("/mfra"))]),
                        ]),
                    ),
                    ("alg", Cbor::text("sha256")),
                    ("hash", Cbor::Bytes(hash.to_vec())),
                    ("name", Cbor::text("jumbf manifest")),
                ]),
            ),
        };
        assertions.push((
            hash_label,
            jumbf::cbor_superbox(jumbf::CBOR, hash_label, &binding.encode()),
        ));

        let created: Vec<Cbor> = assertions
            .iter()
            .map(|(label, superbox)| hashed_uri(label, superbox))
            .collect();
        let claim = Cbor::map([
            ("instanceID", Cbor::text(&self.instance_id)),
            (
                "claim_generator_info",
                Cbor::map([
                    ("name", Cbor::text(plan.generator)),
                    ("version", Cbor::text(plan.generator_version)),
                ]),
            ),
            (
                "signature",
                Cbor::text(format!("self#jumbf=/c2pa/{}/c2pa.signature", self.label)),
            ),
            ("created_assertions", Cbor::Array(created)),
            ("alg", Cbor::text("sha256")),
        ])
        .encode();

        let identity = self.identity;
        let protected = Cbor::Map(vec![
            (Cbor::Int(1), Cbor::Int(identity.cose_algorithm())),
            (
                Cbor::Int(33),
                Cbor::Array(identity.chain().iter().cloned().map(Cbor::Bytes).collect()),
            ),
        ])
        .encode();
        let signature = if signed {
            let to_be_signed = Cbor::Array(vec![
                Cbor::text("Signature1"),
                Cbor::Bytes(protected.clone()),
                Cbor::Bytes(Vec::new()),
                Cbor::Bytes(claim.clone()),
            ])
            .encode();
            identity.sign(&to_be_signed)?
        } else {
            vec![0; identity.signature_len()]
        };
        let cose = Cbor::Tag(
            18,
            Box::new(Cbor::Array(vec![
                Cbor::Bytes(protected),
                Cbor::Map(Vec::new()),
                Cbor::Null,
                Cbor::Bytes(signature),
            ])),
        )
        .encode();

        let assertion_store = jumbf::superbox(
            jumbf::ASSERTION_STORE,
            "c2pa.assertions",
            &assertions.into_iter().map(|(_, b)| b).collect::<Vec<_>>(),
        );
        let manifest = jumbf::superbox(
            jumbf::MANIFEST,
            &self.label,
            &[
                assertion_store,
                jumbf::cbor_superbox(jumbf::CLAIM, "c2pa.claim.v2", &claim),
                jumbf::cbor_superbox(jumbf::SIGNATURE, "c2pa.signature", &cose),
            ],
        );
        Ok(jumbf::superbox(jumbf::MANIFEST_STORE, "c2pa", &[manifest]))
    }

    /// The store's size in a file where it takes `on_disk(len)` bytes from
    /// `start` (its own size changes the exclusion it records): the fixed
    /// point of both.
    fn sized(&self, start: u64, on_disk: impl Fn(u64) -> u64) -> Result<u64, String> {
        let zeros = [0u8; 32];
        let mut excluded = 0;
        for _ in 0..8 {
            let binding = Binding::Data {
                exclusion: (start, excluded),
                hash: &zeros,
            };
            let len = self.store(binding, false)?.len() as u64;
            let next = on_disk(len);
            if next == excluded {
                return Ok(excluded);
            }
            excluded = next;
        }
        Err("The manifest size did not settle.".to_string())
    }
}

fn int(value: u64) -> Result<Cbor, String> {
    i64::try_from(value)
        .map(Cbor::Int)
        .map_err(|_| "The file is too large for a manifest.".to_string())
}

fn hashed_uri(label: &str, superbox: &[u8]) -> Cbor {
    Cbor::map([
        (
            "url",
            Cbor::text(format!("self#jumbf=c2pa.assertions/{label}")),
        ),
        ("hash", Cbor::Bytes(jumbf::superbox_hash(superbox))),
    ])
}

/// Bytes of an APP11 segment's payload: CI `JP`, En, Z and the store's box
/// header repeated in each (ISO/IEC 19566-5 Annex B).
const APP11_OVERHEAD: u64 = 2 + 2 + 4 + 8;
const APP11_CHUNK: u64 = 0xffff - 2 - APP11_OVERHEAD;

/// The bytes the APP11 segments of a `store_len`-byte store take.
fn app11_len(store_len: u64) -> u64 {
    let body = store_len - 8;
    let segments = body.div_ceil(APP11_CHUNK).max(1);
    segments * (2 + 2 + APP11_OVERHEAD) + body
}

fn app11_segments(store: &[u8]) -> Vec<u8> {
    let (header, body) = store.split_at(8);
    let mut out = Vec::with_capacity(app11_len(store.len() as u64) as usize);
    let chunks: Vec<&[u8]> = if body.is_empty() {
        vec![body]
    } else {
        body.chunks(APP11_CHUNK as usize).collect()
    };
    for (i, chunk) in chunks.into_iter().enumerate() {
        out.extend_from_slice(&[0xff, 0xeb]);
        let len = (2 + APP11_OVERHEAD as usize + chunk.len()) as u16;
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(b"JP");
        // Box instance number 1; packet sequence numbers from 1.
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&(i as u32 + 1).to_be_bytes());
        out.extend_from_slice(header);
        out.extend_from_slice(chunk);
    }
    out
}

/// Where a manifest goes in a JPEG: after SOI and the APPn segments that
/// lead the file (JFIF, EXIF, XMP, ICC), before the image segments.
fn jpeg_insert_at(jpeg: &[u8]) -> Result<usize, String> {
    if jpeg.get(0..2) != Some(&[0xff, 0xd8]) {
        return Err("Not a JPEG file.".to_string());
    }
    let mut at = 2;
    loop {
        match jpeg.get(at..at + 4) {
            Some(&[0xff, marker, hi, lo]) if (0xe0..=0xef).contains(&marker) => {
                if marker == 0xeb {
                    return Err("The JPEG already holds JUMBF data.".to_string());
                }
                at += 2 + u16::from_be_bytes([hi, lo]) as usize;
                if at > jpeg.len() {
                    return Err("The JPEG is truncated.".to_string());
                }
            }
            Some(&[0xff, ..]) => return Ok(at),
            _ => return Err("The JPEG is truncated.".to_string()),
        }
    }
}

/// A JPEG with the manifest of `plan` signed by `identity`.
pub fn sign_jpeg(
    jpeg: &[u8],
    plan: &ManifestPlan,
    identity: &SigningIdentity,
) -> Result<Vec<u8>, String> {
    let at = jpeg_insert_at(jpeg)?;
    let manifest = Manifest::new(plan, identity);
    let excluded = manifest.sized(at as u64, app11_len)?;
    // The file without its manifest segments is the JPEG as it was.
    let data_hash = digest::digest(&digest::SHA256, jpeg);
    let binding = Binding::Data {
        exclusion: (at as u64, excluded),
        hash: data_hash.as_ref(),
    };
    let store = manifest.store(binding, true)?;
    let segments = app11_segments(&store);
    if segments.len() as u64 != excluded {
        return Err("The signed manifest changed size.".to_string());
    }
    let mut out = Vec::with_capacity(jpeg.len() + segments.len());
    out.extend_from_slice(&jpeg[..at]);
    out.extend_from_slice(&segments);
    out.extend_from_slice(&jpeg[at..]);
    Ok(out)
}

/// A PNG with the manifest of `plan` signed by `identity`, in a `caBX`
/// chunk right after IHDR (as c2pa-rs places it).
pub fn sign_png(
    png: &[u8],
    plan: &ManifestPlan,
    identity: &SigningIdentity,
) -> Result<Vec<u8>, String> {
    const SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
    if !png.starts_with(SIGNATURE) || png.get(12..16) != Some(b"IHDR") {
        return Err("Not a PNG file.".to_string());
    }
    let ihdr_len = u32::from_be_bytes(png[8..12].try_into().expect("4 bytes")) as usize;
    let at = 8 + 12 + ihdr_len;
    if at > png.len() {
        return Err("The PNG is truncated.".to_string());
    }
    let manifest = Manifest::new(plan, identity);
    // The chunk: length, type, the store, CRC.
    let excluded = manifest.sized(at as u64, |len| len + 12)?;
    let data_hash = digest::digest(&digest::SHA256, png);
    let binding = Binding::Data {
        exclusion: (at as u64, excluded),
        hash: data_hash.as_ref(),
    };
    let store = manifest.store(binding, true)?;
    if store.len() as u64 + 12 != excluded {
        return Err("The signed manifest changed size.".to_string());
    }
    let len = u32::try_from(store.len()).map_err(|_| "The manifest is too large.".to_string())?;
    let mut chunk = Vec::with_capacity(store.len() + 12);
    chunk.extend_from_slice(&len.to_be_bytes());
    chunk.extend_from_slice(b"caBX");
    chunk.extend_from_slice(&store);
    let crc = crc32fast::hash(&chunk[4..]);
    chunk.extend_from_slice(&crc.to_be_bytes());
    let mut out = Vec::with_capacity(png.len() + chunk.len());
    out.extend_from_slice(&png[..at]);
    out.extend_from_slice(&chunk);
    out.extend_from_slice(&png[at..]);
    Ok(out)
}

/// A WebP (extended format, as the export writes it) with the manifest of
/// `plan` signed by `identity`, in a `C2PA` chunk at the end of the RIFF
/// (as c2pa-rs places it); the RIFF size counts it.
pub fn sign_webp(
    webp: &[u8],
    plan: &ManifestPlan,
    identity: &SigningIdentity,
) -> Result<Vec<u8>, String> {
    if webp.len() < 20 || &webp[0..4] != b"RIFF" || &webp[8..12] != b"WEBP" {
        return Err("Not a WebP file.".to_string());
    }
    let riff_len = u32::from_le_bytes(webp[4..8].try_into().expect("4 bytes")) as usize;
    if riff_len + 8 != webp.len() || !webp.len().is_multiple_of(2) {
        return Err("The WebP file's length does not match its RIFF header.".to_string());
    }
    if &webp[12..16] != b"VP8X" {
        return Err("The WebP file is not in the extended format.".to_string());
    }
    let mut at = 12;
    while at + 8 <= webp.len() {
        if &webp[at..at + 4] == b"C2PA" {
            return Err("The WebP file already holds a manifest.".to_string());
        }
        let size = u32::from_le_bytes(webp[at + 4..at + 8].try_into().expect("4 bytes")) as usize;
        at += 8 + size + (size & 1);
    }
    let at = webp.len() as u64;
    let manifest = Manifest::new(plan, identity);
    // The chunk's header and the store; a pad byte after an odd store is
    // outside the exclusion (c2pa-rs `RiffIO`), so it is hashed.
    let excluded = manifest.sized(at, |len| len + 8)?;
    let pad = (excluded % 2) as usize;
    let riff = u32::try_from(riff_len as u64 + excluded + pad as u64)
        .map_err(|_| "The WebP file is too large for a manifest.".to_string())?;
    let mut bound = digest::Context::new(&digest::SHA256);
    bound.update(&webp[..4]);
    bound.update(&riff.to_le_bytes());
    bound.update(&webp[8..]);
    bound.update(&[0u8; 1][..pad]);
    let data_hash = bound.finish();
    let binding = Binding::Data {
        exclusion: (at, excluded),
        hash: data_hash.as_ref(),
    };
    let store = manifest.store(binding, true)?;
    if store.len() as u64 + 8 != excluded {
        return Err("The signed manifest changed size.".to_string());
    }
    let mut out = Vec::with_capacity(webp.len() + store.len() + 9);
    out.extend_from_slice(&webp[..4]);
    out.extend_from_slice(&riff.to_le_bytes());
    out.extend_from_slice(&webp[8..]);
    out.extend_from_slice(b"C2PA");
    out.extend_from_slice(&(store.len() as u32).to_le_bytes());
    out.extend_from_slice(&store);
    out.extend_from_slice(&[0u8; 1][..pad]);
    Ok(out)
}

/// The top-level boxes of a BMFF file: (offset, length).
fn bmff_boxes(file: &[u8]) -> Result<Vec<(usize, usize)>, String> {
    let mut boxes = Vec::new();
    let mut at = 0;
    while at < file.len() {
        let header = file
            .get(at..at + 8)
            .ok_or("The file ends inside a box header.")?;
        let len = match u32::from_be_bytes(header[..4].try_into().expect("4 bytes")) {
            // To the end of the file: a box appended after it would be read
            // as part of it.
            0 => return Err("A box of the file runs to its end.".to_string()),
            1 => {
                let large = file
                    .get(at + 8..at + 16)
                    .ok_or("The file ends inside a box header.")?;
                usize::try_from(u64::from_be_bytes(large.try_into().expect("8 bytes")))
                    .map_err(|_| "A box is too large.".to_string())?
            }
            len => len as usize,
        };
        if len < 8 || file.len() - at < len {
            return Err("A box of the file is truncated.".to_string());
        }
        boxes.push((at, len));
        at += len;
    }
    Ok(boxes)
}

/// The C2PA `uuid` box holding `store`: size, `uuid`, the extended type,
/// version and flags, the purpose `manifest` and the (absent) Merkle
/// offset, then the store.
fn bmff_manifest_box(store: &[u8]) -> Result<Vec<u8>, String> {
    let purpose = b"manifest\0";
    let len = u32::try_from(8 + 16 + 4 + purpose.len() + 8 + store.len())
        .map_err(|_| "The manifest is too large.".to_string())?;
    let mut out = Vec::with_capacity(len as usize);
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(b"uuid");
    out.extend_from_slice(&BMFF_UUID);
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(purpose);
    out.extend_from_slice(&0u64.to_be_bytes());
    out.extend_from_slice(store);
    Ok(out)
}

/// Whether the box `whole` is excluded from the BMFF binding: the C2PA
/// `uuid` box and any `mfra` (C2PA 2.x, `c2pa.hash.bmff.v3`).
fn bmff_excluded(whole: &[u8]) -> bool {
    let kind = &whole[4..8];
    kind == b"mfra" || (kind == b"uuid" && whole.get(8..24) == Some(&BMFF_UUID[..]))
}

/// SHA-256 of each top-level box of `file` but the excluded ones, each
/// preceded by its offset (64-bit big-endian), as `c2pa.hash.bmff.v3`
/// without Merkle trees hashes them.
fn bmff_hash(file: &[u8]) -> Result<digest::Digest, String> {
    let mut hash = digest::Context::new(&digest::SHA256);
    for (at, len) in bmff_boxes(file)? {
        let whole = &file[at..at + len];
        if !bmff_excluded(whole) {
            hash.update(&(at as u64).to_be_bytes());
            hash.update(whole);
        }
    }
    Ok(hash.finish())
}

/// An AVIF with the manifest of `plan` signed by `identity`, in a C2PA
/// `uuid` box right before the first `mdat` (C2PA 2.x: an AVIF without
/// `moov` has its manifest before its media data), the item locations moved
/// with the media data ([`crate::avif_container::insert_before_media`]).
/// Its binding (`c2pa.hash.bmff.v3`) hashes every other top-level box with
/// its offset, in that final layout.
pub fn sign_avif(
    avif: &[u8],
    plan: &ManifestPlan,
    identity: &SigningIdentity,
) -> Result<Vec<u8>, String> {
    let boxes = bmff_boxes(avif)?;
    if avif.get(4..8) != Some(b"ftyp") {
        return Err("Not an AVIF file.".to_string());
    }
    for &(at, len) in &boxes {
        let whole = &avif[at..at + len];
        if bmff_excluded(whole) {
            return Err("The AVIF file already holds a manifest.".to_string());
        }
        if &whole[4..8] == b"moov" {
            return Err("AVIF image sequences cannot be signed.".to_string());
        }
    }
    let manifest = Manifest::new(plan, identity);
    // The store's size does not depend on the hash or the signature's
    // bytes: lay the file out with an unsigned one, then sign it in place.
    let unsigned = manifest.store(Binding::Bmff { hash: &[0; 32] }, false)?;
    let mut file =
        crate::avif_container::insert_before_media(avif, &bmff_manifest_box(&unsigned)?)?;
    let hash = bmff_hash(&file)?;
    let store = manifest.store(
        Binding::Bmff {
            hash: hash.as_ref(),
        },
        true,
    )?;
    let signed = bmff_manifest_box(&store)?;
    let (at, len) = bmff_boxes(&file)?
        .into_iter()
        .find(|&(at, len)| bmff_excluded(&file[at..at + len]))
        .ok_or("The manifest box was not placed.")?;
    if signed.len() != len {
        return Err("The signed manifest changed size.".to_string());
    }
    file[at..at + len].copy_from_slice(&signed);
    Ok(file)
}

/// A TIFF's manifest with its place reserved ([`reserve_tiff`]), to be
/// signed once the hash of every byte before it is known.
pub struct TiffManifest<'a> {
    manifest: Manifest<'a>,
    at: u64,
    len: u32,
}

/// Reserves the manifest of `plan` in a complete TIFF in `file`: tag 0xCD41
/// of a new IFD0 copy, the store last in the file (zeros until
/// [`TiffManifest::sign`]). Every other byte of the file is final then: the
/// header points at the new IFD0.
pub fn reserve_tiff<'a, S: Read + Write + Seek>(
    file: &mut S,
    plan: &'a ManifestPlan,
    identity: &'a SigningIdentity,
) -> Result<TiffManifest<'a>, String> {
    let manifest = Manifest::new(plan, identity);
    let mut sized = Ok(0);
    let (at, len) = crate::tiff_exif::append_reserved_tag(file, TIFF_TAG, |start| {
        sized = manifest.sized(start, |len| len);
        sized
            .clone()
            .ok()
            .and_then(|len| u32::try_from(len).ok())
            .ok_or_else(|| "The manifest could not be sized.".to_string())
    })
    .map_err(|e| format!("The provenance manifest could not be written: {e}"))?;
    sized?;
    Ok(TiffManifest { manifest, at, len })
}

impl TiffManifest<'_> {
    /// Where the store starts: the bytes before it are the bound ones.
    pub fn start(&self) -> u64 {
        self.at
    }

    /// Signs the store, `data_hash` being SHA-256 of the file's first
    /// [`start`](Self::start) bytes, into its place.
    pub fn sign<S: Write + Seek>(self, file: &mut S, data_hash: &[u8]) -> Result<(), String> {
        let binding = Binding::Data {
            exclusion: (self.at, u64::from(self.len)),
            hash: data_hash,
        };
        let store = self.manifest.store(binding, true)?;
        if store.len() as u64 != u64::from(self.len) {
            return Err("The signed manifest changed size.".to_string());
        }
        let mut write = || -> std::io::Result<()> {
            file.seek(SeekFrom::Start(self.at))?;
            file.write_all(&store)?;
            file.seek(SeekFrom::End(0)).map(drop)
        };
        write().map_err(|e| format!("The provenance manifest could not be written: {e}"))
    }
}

/// Signs the manifest of `plan` into a complete TIFF in `file`
/// ([`reserve_tiff`]); the file is read back once for its hash, not held in
/// memory. A streamed export hashes while it writes instead
/// (`crate::signed_tiff`).
pub fn sign_tiff<S: Read + Write + Seek>(
    file: &mut S,
    plan: &ManifestPlan,
    identity: &SigningIdentity,
) -> Result<(), String> {
    let manifest = reserve_tiff(file, plan, identity)?;
    let data_hash = hash_prefix(file, manifest.start()).map_err(|e| e.to_string())?;
    manifest.sign(file, data_hash.as_ref())
}

/// SHA-256 of the first `len` bytes of `file`.
fn hash_prefix<S: Read + Seek>(file: &mut S, len: u64) -> std::io::Result<digest::Digest> {
    file.seek(SeekFrom::Start(0))?;
    let mut context = digest::Context::new(&digest::SHA256);
    let mut buffer = vec![0u8; 1 << 20];
    let mut left = len;
    while left > 0 {
        let want = left.min(buffer.len() as u64) as usize;
        file.read_exact(&mut buffer[..want])?;
        context.update(&buffer[..want]);
        left -= want as u64;
    }
    Ok(context.finish())
}

#[cfg(test)]
mod tests;
