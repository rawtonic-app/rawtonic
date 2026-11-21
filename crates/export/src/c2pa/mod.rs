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
//! - the author, only when the recipe asks for it,
//! - the hard binding: SHA-256 of the whole file except the manifest,
//!   computed after every other byte of the file is written.
//!
//! JPEG (APP11 segments), PNG (a `caBX` chunk) and TIFF (tag 0xCD41) are
//! written; other formats refuse the option ([`supports`]).

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
    matches!(extension, "jpg" | "jpeg" | "png" | "tif" | "tiff")
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

    /// The manifest store for a file whose bytes `exclusion` (start, length)
    /// hold the store, and whose other bytes hash to `data_hash`. Unsigned
    /// (a zero signature of the right length) to find its size.
    fn store(
        &self,
        exclusion: (u64, u64),
        data_hash: &[u8],
        signed: bool,
    ) -> Result<Vec<u8>, String> {
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
            let label = "stds.schema-org.CreativeWork";
            let work = Cbor::map([
                ("@context", Cbor::text("http://schema.org/")),
                ("@type", Cbor::text("CreativeWork")),
                (
                    "author",
                    Cbor::Array(vec![Cbor::map([
                        ("@type", Cbor::text("Person")),
                        ("name", Cbor::text(author)),
                    ])]),
                ),
            ]);
            assertions.push((
                label,
                jumbf::cbor_superbox(jumbf::CBOR, label, &work.encode()),
            ));
        }

        let hash_label = "c2pa.hash.data";
        let binding = Cbor::map([
            (
                "exclusions",
                Cbor::Array(vec![Cbor::map([
                    ("start", int(exclusion.0)?),
                    ("length", int(exclusion.1)?),
                ])]),
            ),
            ("name", Cbor::text("jumbf manifest")),
            ("alg", Cbor::text("sha256")),
            ("hash", Cbor::Bytes(data_hash.to_vec())),
            ("pad", Cbor::Bytes(Vec::new())),
        ]);
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
            let len = self.store((start, excluded), &zeros, false)?.len() as u64;
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
    let store = manifest.store((at as u64, excluded), data_hash.as_ref(), true)?;
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
    let store = manifest.store((at as u64, excluded), data_hash.as_ref(), true)?;
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

/// Signs the manifest of `plan` into a complete TIFF in `file` (tag 0xCD41
/// of a new IFD0 copy, the store last in the file); the file is read back
/// once for its hash, not held in memory.
pub fn sign_tiff<S: Read + Write + Seek>(
    file: &mut S,
    plan: &ManifestPlan,
    identity: &SigningIdentity,
) -> Result<(), String> {
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
    let data_hash = hash_prefix(file, at).map_err(|e| e.to_string())?;
    let store = manifest.store((at, u64::from(len)), data_hash.as_ref(), true)?;
    if store.len() as u64 != u64::from(len) {
        return Err("The signed manifest changed size.".to_string());
    }
    let mut write = || -> std::io::Result<()> {
        file.seek(SeekFrom::Start(at))?;
        file.write_all(&store)?;
        file.seek(SeekFrom::End(0)).map(drop)
    };
    write().map_err(|e| format!("The provenance manifest could not be written: {e}"))
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
