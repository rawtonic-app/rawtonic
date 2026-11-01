//! Import from Lightroom Classic catalogs and Capture One sessions (G50).
//!
//! `rawtonic-interop` reads the other library from a copy (the original is
//! never written); this module translates each photograph's develop
//! settings with the existing converters (Lightroom XMP → presets,
//! Capture One style entries → `costyle`), and registers photographs,
//! metadata, edits, snapshots, virtual copies and albums in the open
//! Rawtonic catalog. The photographs themselves are never written. A
//! Lightroom virtual copy becomes a virtual copy of its photograph (a
//! catalog version with its own edit): its derived editor sidecar
//! (`<photo>.<id>.rrdata`) is published beside a photograph that is there,
//! as for copies made in the app, with a fresh name and never over a file;
//! no XMP is written for copies (Lightroom writes none for them either).
use rawtonic_catalog::{ALBUM, Combine, Field, GROUP, Library, Mode, Op, Rule, SMART};
use rawtonic_interop::{
    Develop, Library as Source, NotRead, Photo, Root, SmartRule, SmartRules, SmartValue,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

fn read_source(path: &Path) -> Result<(Source, &'static str), String> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let result = if name.ends_with(".lrcat") {
        rawtonic_interop::lightroom::read(path).map(|lib| (lib, "lightroom"))
    } else if name.ends_with(".cosessiondb") {
        rawtonic_interop::capture_one::read(path).map(|lib| (lib, "captureOne"))
    } else {
        return Err(
            "Choose a Lightroom Classic catalog (.lrcat) or a Capture One session (.cosessiondb)."
                .into(),
        );
    };
    result.map_err(|e| format!("{e:#}"))
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Relink {
    pub from: PathBuf,
    pub to: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportPreview {
    pub name: String,
    pub kind: &'static str,
    pub photos: usize,
    pub copies: usize,
    pub edited: usize,
    pub albums: usize,
    pub roots: Vec<Root>,
    pub not_read: NotRead,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportReport {
    pub imported: usize,
    pub with_settings: usize,
    pub missing: usize,
    pub copies_skipped: usize,
    /// Virtual copies imported as virtual copies of their photograph (or
    /// found from an earlier import of the same catalog and kept as they
    /// are).
    pub copies: usize,
    /// Edits whose crop reaches beyond the frame Rawtonic develops (sensor
    /// area outside a camera's aspect ratio): the crop keeps its centre and
    /// aspect ratio and is shrunk until all of it shows the photo.
    pub crops_clipped: usize,
    pub albums: usize,
    /// Photographs with Lightroom AI masks (Select Subject, Select Sky):
    /// their model runs when the photo is opened in Develop, so exports
    /// before that leave those masks' layers without effect.
    pub ai_masks_on_open: usize,
    pub errors: Vec<String>,
}

fn preview(source: &Source, kind: &'static str) -> ImportPreview {
    let originals = || source.photos.iter().filter(|p| p.copy_of.is_none());
    ImportPreview {
        name: source.name.clone(),
        kind,
        photos: originals().count(),
        copies: source.photos.len() - originals().count(),
        edited: originals()
            .filter(|p| !matches!(p.develop, Develop::None))
            .count(),
        albums: source
            .albums
            .iter()
            .filter(|a| !a.is_group && a.smart.as_ref().is_none_or(|s| smart_rule(s).is_some()))
            .count(),
        roots: source.roots(),
        not_read: NotRead {
            smart_albums: source.not_read.smart_albums
                + source
                    .albums
                    .iter()
                    .filter(|a| a.smart.as_ref().is_some_and(|s| smart_rule(s).is_none()))
                    .count(),
            ..source.not_read.clone()
        },
    }
}

/// A smart album's rules for a Lightroom smart collection's, or `None`
/// when one of its criteria has no counterpart (the collection is then
/// left out rather than shown with other photographs).
pub fn smart_rule(rules: &SmartRules) -> Option<Rule> {
    let combine = match rules.combine.as_str() {
        "intersect" => Combine::All,
        "union" => Combine::Any,
        "exclude" => Combine::None,
        _ => return None,
    };
    let rules = rules
        .rules
        .iter()
        .map(|rule| match rule {
            SmartRule::Group(group) => smart_rule(group),
            SmartRule::Condition {
                criteria,
                operation,
                value,
                value2,
                units,
            } => smart_condition(criteria, operation, value, value2, units.as_deref()),
        })
        .collect::<Option<Vec<_>>>()?;
    Some(Rule::Group { combine, rules })
}

fn smart_condition(
    criteria: &str,
    operation: &str,
    value: &SmartValue,
    value2: &SmartValue,
    units: Option<&str>,
) -> Option<Rule> {
    let number = |v: &SmartValue| match v {
        SmartValue::Number(n) => Some(*n),
        _ => None,
    };
    let text = |v: &SmartValue| match v {
        SmartValue::Text(t) => Some(t.clone()),
        _ => None,
    };
    let compare = |op: &str| match op {
        "==" => Some(Op::Is),
        "!=" => Some(Op::IsNot),
        ">" => Some(Op::Gt),
        ">=" => Some(Op::Ge),
        "<" => Some(Op::Lt),
        "<=" => Some(Op::Le),
        _ => None,
    };
    let (field, op, value) = match criteria {
        "rating" => (Field::Rating, compare(operation)?, json!(number(value)?)),
        "pick" => {
            let flag = match number(value)? as i64 {
                1 => "picked",
                0 => "unflagged",
                -1 => "rejected",
                _ => return None,
            };
            let op = compare(operation).filter(|op| matches!(op, Op::Is | Op::IsNot))?;
            (Field::Flag, op, json!(flag))
        }
        "labelColor" => {
            let label = match number(value)? as i64 {
                1 => "red",
                2 => "yellow",
                3 => "green",
                4 => "blue",
                5 => "purple",
                _ => return None,
            };
            let op = compare(operation).filter(|op| matches!(op, Op::Is | Op::IsNot))?;
            (Field::Label, op, json!(label))
        }
        "keywords" => {
            let op = match operation {
                "any" => Op::ContainsAny,
                "all" | "words" => Op::Contains,
                "noneOf" => Op::NotContains,
                "beginsWith" => Op::StartsWith,
                "endsWith" => Op::EndsWith,
                "empty" => Op::Empty,
                "notEmpty" => Op::NotEmpty,
                _ => return None,
            };
            let value = match op {
                Op::Empty | Op::NotEmpty => Value::Null,
                _ => json!(text(value)?),
            };
            (Field::Keyword, op, value)
        }
        "captureTime" => match operation {
            "in" => (
                Field::CaptureDate,
                Op::Between,
                json!([text(value)?, text(value2)?]),
            ),
            "inLast" | "notInLast" => {
                let unit = units?;
                if !matches!(unit, "days" | "weeks" | "months" | "years") {
                    return None;
                }
                let op = if operation == "inLast" {
                    Op::InLast
                } else {
                    Op::NotInLast
                };
                let amount = number(value)?;
                (
                    Field::CaptureDate,
                    op,
                    json!({"amount": amount as u64, "unit": unit}),
                )
            }
            _ => (Field::CaptureDate, compare(operation)?, json!(text(value)?)),
        },
        "fileFormat" => {
            let kind = match text(value)?.as_str() {
                "RAW" => "raw",
                "DNG" => "dng",
                "JPG" => "jpeg",
                "TIFF" => "tiff",
                "PNG" => "png",
                "HEIC" => "heif",
                _ => return None,
            };
            let op = compare(operation).filter(|op| matches!(op, Op::Is | Op::IsNot))?;
            (Field::FileType, op, json!(kind))
        }
        _ => return None,
    };
    Some(Rule::Condition { field, op, value })
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Rawtonic adjustments for another editor's settings, or `None`.
pub fn translate(develop: &Develop) -> Option<Value> {
    let adjustments = match develop {
        Develop::None => return None,
        Develop::LightroomXmp(xmp) => {
            crate::preset_converter::convert_xmp_to_preset(xmp)
                .ok()?
                .adjustments
        }
        Develop::CaptureOne(entries) => {
            let style: String = entries
                .iter()
                .map(|(k, v)| format!(r#"<E K="{}" V="{}"/>"#, escape(k), escape(v)))
                .collect();
            crate::costyle::convert_costyle_to_preset(&format!("<SL>{style}</SL>"), "Import")
                .ok()?
                .adjustments
        }
    };
    adjustments
        .as_object()
        .is_some_and(|a| !a.is_empty())
        .then_some(adjustments)
}

/// Registers the photographs that exist, with metadata, translated edits
/// and albums, in `catalog`.
pub fn import_into(catalog: &Library, source: &Source) -> Result<ImportReport, String> {
    if catalog.manifest.mode != Mode::Catalog {
        return Err("Open a Rawtonic catalog to import into: sessions hold only photographs inside their own folder.".into());
    }
    let mut report = ImportReport::default();
    let mut assets: HashMap<&str, String> = HashMap::new();
    let _batch = catalog.import_batch(rawtonic_catalog::ImportKind::Add);
    for photo in source.photos.iter().filter(|p| p.copy_of.is_none()) {
        if !photo.path.is_file() {
            report.missing += 1;
            continue;
        }
        match import_photo(catalog, photo) {
            Ok((asset, imported)) => {
                report.imported += 1;
                report.with_settings += usize::from(imported.translated);
                report.ai_masks_on_open += usize::from(imported.ai_masks);
                report.crops_clipped += imported.crops_clipped;
                assets.insert(photo.key.as_str(), asset);
            }
            Err(e) => report.errors.push(format!("{}: {e}", photo.path.display())),
        }
    }
    // Virtual copies, once their photograph is in.
    for photo in source.photos.iter().filter(|p| p.copy_of.is_some()) {
        let Some(asset) = photo
            .copy_of
            .as_deref()
            .and_then(|master| assets.get(master))
        else {
            report.copies_skipped += 1;
            continue;
        };
        match import_copy(catalog, asset, &source.name, photo) {
            Ok(imported) => {
                report.copies += 1;
                report.ai_masks_on_open += usize::from(imported.ai_masks);
                report.crops_clipped += imported.crops_clipped;
            }
            Err(e) => {
                report.copies_skipped += 1;
                report.errors.push(format!("{}: {e}", photo.path.display()));
            }
        }
    }
    // Albums and groups, parents first; existing ones (a repeated import)
    // are reused.
    let existing = catalog.collections().map_err(|e| e.to_string())?;
    let mut created: HashMap<&str, String> = HashMap::new();
    let mut pending: Vec<&rawtonic_interop::Album> = source.albums.iter().collect();
    while !pending.is_empty() {
        let before = pending.len();
        let mut next = Vec::new();
        for album in pending {
            let parent = match &album.parent {
                Some(p) => match created.get(p.as_str()) {
                    Some(id) => Some(id.clone()),
                    None if source.albums.iter().any(|a| &a.key == p) => {
                        next.push(album);
                        continue;
                    }
                    None => None,
                },
                None => None,
            };
            let smart = match &album.smart {
                Some(rules) => match smart_rule(rules) {
                    Some(rule) => Some(rule),
                    // Announced in the preview as not imported.
                    None => continue,
                },
                None => None,
            };
            let kind = if album.is_group {
                GROUP
            } else if smart.is_some() {
                SMART
            } else {
                ALBUM
            };
            // A smart album is reused only with the same rules: one of the
            // same name with other rules (changed in Lightroom since the
            // last import, or made here) stays as it is beside a new one.
            let id = match existing.iter().find(|c| {
                c.name == album.name
                    && c.parent == parent
                    && c.kind == kind
                    && (smart.is_none() || c.rules.as_ref() == smart.as_ref())
            }) {
                Some(c) => c.id.clone(),
                None => match &smart {
                    Some(rule) => {
                        catalog
                            .create_smart_collection(&album.name, parent.as_deref(), rule)
                            .map_err(|e| e.to_string())?
                            .id
                    }
                    None => {
                        catalog
                            .create_collection_of_kind(&album.name, parent.as_deref(), kind)
                            .map_err(|e| e.to_string())?
                            .id
                    }
                },
            };
            if smart.is_some() {
                report.albums += 1;
            } else if !album.is_group {
                let members: Vec<String> = album
                    .photos
                    .iter()
                    .filter_map(|key| {
                        let key = source
                            .photos
                            .iter()
                            .find(|p| &p.key == key)
                            .map(|p| p.copy_of.as_deref().unwrap_or(&p.key))?;
                        assets.get(key).cloned()
                    })
                    .collect();
                if !members.is_empty() {
                    catalog
                        .add_to_collection(&id, &members)
                        .map_err(|e| e.to_string())?;
                }
                report.albums += 1;
            }
            created.insert(album.key.as_str(), id);
        }
        if next.len() == before {
            break;
        }
        pending = next;
    }
    Ok(report)
}

/// Whether the translated edit holds AI masks whose data a model computes
/// when the photo is opened (`ai-foreground`, `ai-sky` without data).
fn has_ai_masks_on_open(adjustments: &Value) -> bool {
    adjustments["masks"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|layer| layer["subMasks"].as_array())
        .flatten()
        .any(|sub| {
            matches!(sub["type"].as_str(), Some("ai-foreground" | "ai-sky"))
                && sub["parameters"]["maskDataBase64"].is_null()
        })
}

/// What the import made of one photograph's (or copy's) edits.
#[derive(Debug, Default)]
struct Imported {
    translated: bool,
    /// Its edit waits for a model when opened (`has_ai_masks_on_open`).
    ai_masks: bool,
    /// Edits (its own and its snapshots') whose crop was clipped.
    crops_clipped: usize,
}

/// The Rawtonic recipe for one of the photo's edits in the other editor
/// (its current settings, a virtual copy's or a snapshot's), and whether
/// its crop was clipped to the developed frame.
///
/// It starts with the versions a new edit of that photo starts with in the
/// editor (current render, sharpening and frame versions, the current
/// fringe and lateral CA passes), so it develops with the engine a new
/// edit gets (a recipe without `renderVersion` renders as version 1). Its
/// lens correction is the default one (`embedded_lens::apply_default`,
/// which every render and the editor apply to a recipe without a lens
/// mode) as Lightroom's lens profile switch and amounts decide
/// (`lightroom_lens`); its crop and straightening become `crop` and
/// `rotation` in the photo's own frame (`rawtonic_engine::lightroom_crop`).
fn recipe_for(develop: &Develop, path: &Path) -> Option<(Value, bool)> {
    let mut adjustments = translate(develop)?;
    let is_raw = crate::formats::is_raw_file(path);
    let mut base = json!({});
    rawtonic_engine::image_processing::apply_render_default(&mut base);
    rawtonic_engine::edge_fringe::apply_default(&mut base, true, is_raw);
    rawtonic_engine::lateral_ca::apply_default(&mut base, true, is_raw);
    if let (Some(map), Some(base)) = (adjustments.as_object_mut(), base.as_object()) {
        for (key, value) in base {
            map.entry(key.clone()).or_insert_with(|| value.clone());
        }
        // The other editor's geometry (spots, masks, red eyes; Lightroom keeps
        // it normalised in the camera's frame of the raw) is that of the
        // newest frame of the RAW (`source_frame`): the edit is made on it,
        // as a new edit would be.
        if is_raw {
            map.insert(
                rawtonic_engine::source_frame::KEY.to_string(),
                json!(rawtonic_engine::source_frame::VERSION),
            );
        }
    }
    let mut clipped = false;
    if let Develop::LightroomXmp(xmp) = develop {
        let attrs = crate::preset_converter::crs_attributes(xmp);
        lightroom_lens(&attrs, path, &mut adjustments);
        clipped = lightroom_crop(&attrs, path, &mut adjustments);
    }
    // Lightroom's spots, red eye corrections and mask geometry render from
    // its own geometry once they know the photo's orientation, a fact of
    // the file (`rawtonic_engine::lightroom_retouch`, `lightroom_masks`):
    // exports of photos never opened keep them.
    if (rawtonic_engine::lightroom_retouch::needs_orientation(&adjustments)
        || rawtonic_engine::lightroom_masks::needs_orientation(&adjustments))
        && let Ok(bytes) = std::fs::read(path)
    {
        let orientation = crate::image_loader::source_orientation(&bytes, &path.to_string_lossy());
        rawtonic_engine::lightroom_retouch::set_orientation(&mut adjustments, orientation);
        rawtonic_engine::lightroom_masks::set_orientation(&mut adjustments, orientation);
    }
    Some((adjustments, clipped))
}

/// Lightroom's lens profile corrections on Rawtonic's own lens data
/// (Adobe's profiles are not used: notes agent-knowledge/adobe-lens-profiles).
/// On: the correction a new edit gets (left to the default every render
/// applies to a recipe without a lens mode) or, where that has none (DSLR
/// lenses), the lens's lensfun profile found from the photo's EXIF as the
/// Lens tool's auto-detect finds it; with Lightroom's Distortion and
/// Vignetting amounts (both 0–200 %, 100 = the profile's, as Rawtonic's).
/// Off: no profile correction — except the camera's own distortion data
/// (Olympus/OM, Panasonic, Fujifilm, Nikon Z, Sony compacts), which
/// Lightroom applies to those files whether the switch is on or not; their
/// vignetting is a profile correction there (off).
fn lightroom_lens(attrs: &HashMap<String, String>, path: &Path, adjustments: &mut Value) {
    let Some(map) = adjustments.as_object_mut() else {
        return;
    };
    if map.contains_key("lensCorrectionMode") {
        return;
    }
    let on = attrs
        .get("LensProfileEnable")
        .is_some_and(|v| v.trim() == "1");
    if on {
        let amount = |key: &str| {
            attrs
                .get(key)
                .and_then(|v| v.trim().trim_start_matches('+').parse::<f64>().ok())
                .filter(|v| v.is_finite())
                .map_or(100.0, |v| v.clamp(0.0, 200.0))
        };
        for (key, lr) in [
            ("lensDistortionAmount", "LensProfileDistortionScale"),
            ("lensVignetteAmount", "LensProfileVignettingScale"),
        ] {
            let value = amount(lr);
            if value != 100.0 {
                map.insert(key.into(), json!(value));
            }
        }
    }
    // Which correction would the default be?
    let mut default = json!({});
    rawtonic_engine::embedded_lens::apply_default(&mut default, path);
    if on {
        if default.get("lensCorrectionMode").is_none()
            && let Some((maker, model, params)) =
                rawtonic_engine::embedded_lens::lensfun_for_photo(path)
            && let Ok(params) = serde_json::to_value(params)
        {
            map.insert("lensCorrectionMode".into(), json!("auto"));
            map.insert("lensMaker".into(), json!(maker));
            map.insert("lensModel".into(), json!(model));
            map.insert("lensDistortionParams".into(), params);
        }
        return;
    }
    match default["lensCorrectionMode"].as_str() {
        Some(rawtonic_engine::embedded_lens::CAMERA_MODE) => {
            if let Some(default) = default.as_object() {
                for (key, value) in default {
                    if key.starts_with("lens") || key.starts_with("camera") {
                        map.insert(key.clone(), value.clone());
                    }
                }
            }
            map.insert("lensVignetteEnabled".into(), json!(false));
        }
        Some(_) => {
            map.insert("lensCorrectionMode".into(), json!("manual"));
            map.insert("lensDistortionParams".into(), Value::Null);
        }
        None => {}
    }
}

/// Lightroom's crop and straightening as `crop`, `rotation` and the crop's
/// locked `aspectRatio` (`rawtonic_engine::lightroom_crop`). Returns
/// whether the crop reached beyond the developed frame and was clipped.
fn lightroom_crop(attrs: &HashMap<String, String>, path: &Path, adjustments: &mut Value) -> bool {
    let number = |key: &str, default: f64| {
        attrs
            .get(key)
            .and_then(|v| v.trim().trim_start_matches('+').parse::<f64>().ok())
            .filter(|v| v.is_finite())
            .unwrap_or(default)
    };
    if attrs
        .get("HasCrop")
        .is_some_and(|v| v.trim().eq_ignore_ascii_case("false"))
    {
        return false;
    }
    let crop = rawtonic_engine::lightroom_crop::LightroomCrop {
        left: number("CropLeft", 0.0),
        top: number("CropTop", 0.0),
        right: number("CropRight", 1.0),
        bottom: number("CropBottom", 1.0),
        angle: number("CropAngle", 0.0),
    };
    if crop.is_whole() {
        return false;
    }
    let placed = if crate::formats::is_raw_file(path) {
        rawtonic_engine::lightroom_crop::place_for_file(path, &crop)
    } else {
        // Other images: the whole stored image is the frame.
        let orientation = std::fs::read(path)
            .ok()
            .map(|bytes| crate::image_loader::source_orientation(&bytes, &path.to_string_lossy()));
        image::image_dimensions(path).ok().and_then(|(w, h)| {
            let whole = [0.0, 0.0, f64::from(w), f64::from(h)];
            rawtonic_engine::lightroom_crop::place(&crop, whole, whole, orientation.unwrap_or(1))
        })
    };
    let (Some(placed), Some(map)) = (placed, adjustments.as_object_mut()) else {
        return false;
    };
    map.insert("crop".into(), placed.crop);
    map.insert("rotation".into(), json!(placed.rotation));
    map.insert("aspectRatio".into(), json!(placed.aspect_ratio));
    placed.clipped
}

/// Registers one photograph with its translated edit and snapshots.
fn import_photo(catalog: &Library, photo: &Photo) -> anyhow::Result<(String, Imported)> {
    let recipe = recipe_for(&photo.develop, &photo.path);
    let mut imported = Imported {
        translated: recipe.is_some(),
        ai_masks: recipe
            .as_ref()
            .is_some_and(|(adjustments, _)| has_ai_masks_on_open(adjustments)),
        crops_clipped: recipe
            .as_ref()
            .map_or(0, |(_, clipped)| usize::from(*clipped)),
    };
    let adjustments = recipe.map(|(adjustments, _)| adjustments);
    // The photograph, its translated edit and its metadata are stored in one
    // transaction: a failure leaves none of them, a retry imports it whole.
    let contents = rawtonic_catalog::Contents {
        editor_state: adjustments.as_ref().map(|adjustments| {
            json!({
                "version": crate::image_processing::ImageMetadata::default().version,
                "rating": photo.metadata.rating,
                "adjustments": adjustments,
            })
        }),
        metadata: Some(photo.metadata.clone()),
        xmp_digest: None,
    };
    let id = match catalog.asset_at(&photo.path)? {
        Some(asset) => {
            catalog.apply_contents(&asset.id, &contents)?;
            asset.id
        }
        None => {
            let planned = catalog.plan_add(&photo.path, contents.clone())?;
            let (id, added) = catalog
                .insert_planned(vec![planned])?
                .pop()
                .ok_or_else(|| anyhow::anyhow!("Nothing was registered"))?;
            if !added {
                catalog.apply_contents(&id, &contents)?;
            }
            id
        }
    };
    let version = catalog.version_for(&id, None)?;
    imported.crops_clipped += import_snapshots(catalog, version.as_deref(), photo)?;
    Ok((id, imported))
}

/// Takes over the photograph's (or copy's) snapshots as the snapshots of
/// `version`, each translated like its edit, oldest first — unless the
/// version has snapshots already (its own, or a repeated import's).
/// Returns how many had their crop clipped.
fn import_snapshots(
    catalog: &Library,
    version: Option<&str>,
    photo: &Photo,
) -> anyhow::Result<usize> {
    let Some(version) = version else {
        return Ok(0);
    };
    if photo.snapshots.is_empty() || !catalog.snapshots(version)?.is_empty() {
        return Ok(0);
    }
    let mut clipped = 0;
    for (name, develop) in &photo.snapshots {
        // A snapshot at Lightroom's defaults is the photo as it develops
        // without an edit.
        let (adjustments, was_clipped) =
            recipe_for(develop, &photo.path).unwrap_or((json!({}), false));
        clipped += usize::from(was_clipped);
        catalog.add_snapshot(version, name, adjustments)?;
    }
    Ok(clipped)
}

/// The id the virtual copy with the source identity `origin` (the other
/// editor's global id for it: Lightroom's image UUID, else the library's
/// name and the copy's key there) of the asset `asset` gets: derived from
/// both, so a repeated import of that library (or of a copy of it) finds
/// the copy it made, and another library's copy of the same photo gets
/// another id (`attempt` > 0 when that name is taken by a file the catalog
/// does not know).
fn copy_id(asset: &str, origin: &str, attempt: u32) -> String {
    use sha2::Digest;
    let digest = sha2::Sha256::digest(format!("imported-copy\0{asset}\0{origin}\0{attempt}"));
    digest.iter().take(3).map(|b| format!("{b:02x}")).collect()
}

/// A copy's source identity for [`copy_id`].
fn copy_origin(source_name: &str, photo: &Photo) -> String {
    match &photo.uid {
        Some(uid) => format!("uid\0{uid}"),
        None => format!("key\0{source_name}\0{}", photo.key),
    }
}

/// Registers a virtual copy of the asset `asset` with its own edit,
/// metadata and snapshots, and publishes its derived editor sidecar beside
/// the photograph when that is there (never over a file). A copy an earlier
/// import made is kept as it is (it may have been edited since).
fn import_copy(
    catalog: &Library,
    asset: &str,
    source_name: &str,
    photo: &Photo,
) -> anyhow::Result<Imported> {
    let known = catalog.copies(asset)?;
    let origin = copy_origin(source_name, photo);
    let name = photo
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| anyhow::anyhow!("No file name"))?;
    let sidecar_of = |id: &str| photo.path.with_file_name(format!("{name}.{id}.rrdata"));
    let mut id = None;
    for attempt in 0..16 {
        let candidate = copy_id(asset, &origin, attempt);
        if known.contains(&candidate) {
            // Imported before: kept as it is. Its derived sidecar is
            // published again if it is missing (an earlier import could not
            // write beside the photograph), so listings find the copy.
            let sidecar = sidecar_of(&candidate);
            if photo.path.is_file()
                && !sidecar.exists()
                && let Some(state) = catalog.copy_state(asset, &candidate)?
            {
                publish_new(&sidecar, &serde_json::to_vec_pretty(&state)?)?;
            }
            return Ok(Imported::default());
        }
        if !sidecar_of(&candidate).exists() {
            id = Some(candidate);
            break;
        }
    }
    let id = id.ok_or_else(|| anyhow::anyhow!("No free name for the virtual copy"))?;
    let recipe = recipe_for(&photo.develop, &photo.path);
    let mut imported = Imported {
        translated: recipe.is_some(),
        ai_masks: recipe
            .as_ref()
            .is_some_and(|(adjustments, _)| has_ai_masks_on_open(adjustments)),
        crops_clipped: recipe
            .as_ref()
            .map_or(0, |(_, clipped)| usize::from(*clipped)),
    };
    let mut state = json!({
        "version": crate::image_processing::ImageMetadata::default().version,
        "rating": photo.metadata.rating,
        "tags": [],
        "adjustments": recipe.map_or(Value::Null, |(adjustments, _)| adjustments),
    });
    rawtonic_catalog::apply_editor_metadata(&mut state, photo.metadata.clone());
    catalog.save_copy_state(asset, &id, &state)?;
    let version = catalog.version_for(asset, Some(&id))?;
    imported.crops_clipped += import_snapshots(catalog, version.as_deref(), photo)?;
    if photo.path.is_file()
        && let Some(state) = catalog.copy_state(asset, &id)?
    {
        publish_new(&sidecar_of(&id), &serde_json::to_vec_pretty(&state)?)?;
    }
    Ok(imported)
}

/// Writes `bytes` as the new file `path`: a synced temporary file published
/// under that name only if nothing is there.
fn publish_new(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    use std::io::Write;
    let mut file = rawtonic_metadata::temp::file_for(path)?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    rawtonic_metadata::no_clobber::persist(file, path)?;
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        std::fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

#[tauri::command]
pub async fn library_import_preview(
    source: String,
    relinks: Vec<Relink>,
) -> Result<ImportPreview, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let (mut lib, kind) = read_source(Path::new(&source))?;
        for relink in &relinks {
            lib.relink(&relink.from, &relink.to);
        }
        Ok(preview(&lib, kind))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn library_import(
    source: String,
    directory: String,
    relinks: Vec<Relink>,
) -> Result<ImportReport, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let (mut lib, _) = read_source(Path::new(&source))?;
        for relink in &relinks {
            lib.relink(&relink.from, &relink.to);
        }
        let catalog = Library::open(Path::new(&directory)).map_err(|e| e.to_string())?;
        let report = import_into(&catalog, &lib);
        // The imported photographs' capture facts, for the search.
        crate::library_search::start_facts_pass(catalog.directory.clone());
        report
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    use rawtonic_interop::Album;
    use rawtonic_metadata::Metadata;
    use std::fs;

    fn photo(key: &str, path: PathBuf, develop: Develop, copy_of: Option<&str>) -> Photo {
        Photo {
            key: key.into(),
            path,
            metadata: Metadata {
                rating: 4,
                label: "Red".into(),
                flag: 1,
                keywords: vec!["Places|Rome".into()],
                title: "Summer".into(),
                ..Default::default()
            },
            develop,
            copy_of: copy_of.map(str::to_string),
            copy_name: None,
            snapshots: Vec::new(),
            uid: None,
        }
    }

    #[test]
    fn lightroom_and_capture_one_settings_translate() {
        let lr = translate(&Develop::LightroomXmp(
            r#"<rdf:Description xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" crs:Exposure2012="+0.50" crs:Contrast2012="+10"/>"#.into(),
        ))
        .unwrap();
        assert_eq!(lr["exposure"], 0.5);
        let c1 = translate(&Develop::CaptureOne(vec![
            ("Exposure".into(), "0.7".into()),
            ("HighlightRecoveryEx".into(), "-100".into()),
        ]))
        .unwrap();
        assert_eq!(c1["exposure"], 0.7);
        assert!(c1["highlights"].as_f64().unwrap() < -40.0, "{c1}");
        assert!(translate(&Develop::None).is_none());
    }

    #[test]
    fn photos_metadata_edits_and_albums_land_in_the_catalog_and_repeat_safely() {
        let tmp = tempfile::tempdir().unwrap();
        let catalog = Library::create(&tmp.path().join("Rawtonic"), "Main", Mode::Catalog).unwrap();
        let photos = tmp.path().join("Photos");
        fs::create_dir_all(&photos).unwrap();
        fs::write(photos.join("A.NEF"), b"a").unwrap();
        let before = fs::read_dir(&photos).unwrap().count();
        let source = Source {
            name: "LR".into(),
            photos: vec![
                photo(
                    "1",
                    photos.join("A.NEF"),
                    Develop::LightroomXmp(r#"crs:Exposure2012="+0.50""#.into()),
                    None,
                ),
                photo("2", photos.join("Gone.NEF"), Develop::None, None),
                photo("3", photos.join("A.NEF"), Develop::None, Some("1")),
            ],
            albums: vec![
                Album {
                    key: "10".into(),
                    name: "Trips".into(),
                    parent: None,
                    is_group: true,
                    photos: vec![],
                    smart: None,
                },
                Album {
                    key: "11".into(),
                    name: "Italy".into(),
                    parent: Some("10".into()),
                    is_group: false,
                    photos: vec!["1".into(), "3".into()],
                    smart: None,
                },
                Album {
                    key: "12".into(),
                    name: "Best picks".into(),
                    parent: Some("10".into()),
                    is_group: false,
                    photos: vec![],
                    smart: Some(SmartRules {
                        combine: "intersect".into(),
                        rules: vec![
                            condition("rating", ">=", SmartValue::Number(4.0)),
                            condition("pick", "==", SmartValue::Number(1.0)),
                        ],
                    }),
                },
                // Edit time has no counterpart: left out, announced.
                Album {
                    key: "13".into(),
                    name: "Recently edited".into(),
                    parent: None,
                    is_group: false,
                    photos: vec![],
                    smart: Some(SmartRules {
                        combine: "intersect".into(),
                        rules: vec![condition("touchTime", "inLast", SmartValue::Number(2.0))],
                    }),
                },
            ],
            not_read: NotRead::default(),
        };
        let shown = preview(&source, "lightroom");
        assert_eq!((shown.albums, shown.not_read.smart_albums), (2, 1));
        let report = import_into(&catalog, &source).unwrap();
        assert_eq!(
            (
                report.imported,
                report.with_settings,
                report.missing,
                report.copies_skipped,
                report.copies,
                report.albums
            ),
            (1, 1, 1, 0, 1, 2)
        );
        let asset = catalog.asset_at(&photos.join("A.NEF")).unwrap().unwrap();
        let meta = catalog.metadata(&asset.id).unwrap();
        assert_eq!((meta.rating, meta.label.as_str(), meta.flag), (4, "Red", 1));
        assert_eq!(meta.keywords, vec!["Places|Rome".to_string()]);
        assert_eq!(meta.title, "Summer");
        assert_eq!(
            catalog.editor_state(&asset.id).unwrap().unwrap()["adjustments"]["exposure"],
            0.5
        );
        let collections = catalog.collections().unwrap();
        let italy = collections.iter().find(|c| c.name == "Italy").unwrap();
        let trips = collections.iter().find(|c| c.name == "Trips").unwrap();
        assert_eq!(italy.parent.as_deref(), Some(trips.id.as_str()));
        assert_eq!(catalog.collection_assets(&italy.id).unwrap().len(), 1);
        let best = collections.iter().find(|c| c.name == "Best picks").unwrap();
        assert_eq!((best.kind.as_str(), best.count), (SMART, 1));
        let best_id = best.id.clone();
        assert_eq!(best.parent.as_deref(), Some(trips.id.as_str()));
        assert!(!collections.iter().any(|c| c.name == "Recently edited"));
        // The virtual copy is a copy of the photograph with its own
        // metadata; beside the photograph only its derived editor sidecar
        // was published (the photograph itself is untouched).
        let copies = catalog.copies(&asset.id).unwrap();
        assert_eq!(
            copies,
            vec![copy_id(&asset.id, &copy_origin("LR", &source.photos[2]), 0)]
        );
        let copy = catalog.copy_state(&asset.id, &copies[0]).unwrap().unwrap();
        assert_eq!(copy["rating"], 4);
        assert!(copy["adjustments"].is_null());
        assert_eq!(fs::read(photos.join("A.NEF")).unwrap(), b"a");
        let sidecar = photos.join(format!("A.NEF.{}.rrdata", copies[0]));
        assert_eq!(fs::read_dir(&photos).unwrap().count(), before + 1);
        let derived: Value = serde_json::from_slice(&fs::read(&sidecar).unwrap()).unwrap();
        assert_eq!(derived["rating"], 4);
        // A second import changes nothing: the copy is found, not doubled.
        let again = import_into(&catalog, &source).unwrap();
        assert_eq!(again.copies, 1);
        assert_eq!(catalog.copies(&asset.id).unwrap(), copies);
        assert_eq!(fs::read_dir(&photos).unwrap().count(), before + 1);
        assert_eq!(catalog.collections().unwrap().len(), collections.len());
        assert_eq!(catalog.assets(None).unwrap().len(), 1);
        // Rules changed in Lightroom since: a new smart album beside the
        // earlier one, which keeps its rules.
        let mut changed = source.clone();
        changed.albums[2].smart = Some(SmartRules {
            combine: "intersect".into(),
            rules: vec![condition("rating", ">=", SmartValue::Number(5.0))],
        });
        import_into(&catalog, &changed).unwrap();
        let after = catalog.collections().unwrap();
        let best: Vec<_> = after.iter().filter(|c| c.name == "Best picks").collect();
        assert_eq!(best.len(), 2);
        assert!(best.iter().any(|c| c.id == best_id && c.count == 1));
        assert!(best.iter().any(|c| c.id != best_id && c.count == 0));
    }

    /// A Lightroom edit arrives as a new edit of the photo would start
    /// (current render version), with its Kelvin white balance taken as it
    /// is, its crop in the photo's pixels, its snapshots as snapshots.
    #[test]
    fn lightroom_white_balance_crop_and_snapshots_are_imported() {
        let tmp = tempfile::tempdir().unwrap();
        let catalog = Library::create(&tmp.path().join("Rawtonic"), "Main", Mode::Catalog).unwrap();
        let photos = tmp.path().join("Photos");
        fs::create_dir_all(&photos).unwrap();
        let path = photos.join("A.png");
        image::RgbImage::new(100, 80).save(&path).unwrap();
        let xmp = |extra: &str| {
            Develop::LightroomXmp(format!(
                r#"crs:WhiteBalance="Custom" crs:Temperature="3200" crs:Tint="+5" crs:HasCrop="True" crs:CropLeft="0.1" crs:CropTop="0.2" crs:CropRight="0.9" crs:CropBottom="0.8" crs:CropAngle="0" {extra}"#
            ))
        };
        let mut lr = photo("1", path.clone(), xmp(r#"crs:Exposure2012="+0.50""#), None);
        lr.snapshots = vec![(
            "Before".into(),
            Develop::LightroomXmp(
                r#"crs:WhiteBalance="As Shot" crs:Temperature="5150" crs:Tint="+9""#.into(),
            ),
        )];
        let mut copy = photo(
            "2",
            path.clone(),
            xmp(r#"crs:Exposure2012="-1.00""#),
            Some("1"),
        );
        copy.metadata.rating = 2;
        let source = Source {
            name: "LR".into(),
            photos: vec![lr, copy],
            albums: vec![],
            not_read: NotRead::default(),
        };
        let report = import_into(&catalog, &source).unwrap();
        assert_eq!(
            (report.imported, report.copies, report.crops_clipped),
            (1, 1, 0)
        );
        let asset = catalog.asset_at(&path).unwrap().unwrap();
        let a = catalog.editor_state(&asset.id).unwrap().unwrap()["adjustments"].clone();
        assert_eq!(a["whiteBalance"], "custom");
        assert_eq!(
            (a["wbTemperature"].as_f64(), a["wbTint"].as_f64()),
            (Some(3200.0), Some(5.0))
        );
        assert!(a.get("temperature").is_none());
        assert_eq!(
            a["renderVersion"].as_u64(),
            Some(rawtonic_engine::image_processing::RENDER_VERSION)
        );
        assert_eq!(
            a["crop"],
            json!({"unit": "px", "x": 10.0, "y": 16.0, "width": 80.0, "height": 48.0})
        );
        assert_eq!(a["rotation"], 0.0);
        let version = catalog.version_for(&asset.id, None).unwrap().unwrap();
        let snapshots = catalog.snapshots(&version).unwrap();
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].name, "Before");
        // As Shot keeps the camera's balance: Lightroom's Temperature beside
        // it is only its readout.
        assert_eq!(snapshots[0].adjustments["whiteBalance"], "asShot");
        assert!(snapshots[0].adjustments.get("wbTemperature").is_none());
        // The copy has its own edit and rating; the photo's stays.
        let id = copy_id(&asset.id, &copy_origin("LR", &source.photos[1]), 0);
        let state = catalog.copy_state(&asset.id, &id).unwrap().unwrap();
        assert_eq!(state["adjustments"]["exposure"], -1.0);
        assert_eq!(state["rating"], 2);
        assert_eq!(a["exposure"], 0.5);
        // Snapshots are not doubled by a repeated import.
        import_into(&catalog, &source).unwrap();
        assert_eq!(catalog.snapshots(&version).unwrap().len(), 1);
    }

    /// Copies of the same photo from two libraries stay two copies (their
    /// identity is the source's global id); a copy whose sidecar could not
    /// be written beside the photograph gets it on the next import.
    #[cfg(unix)]
    #[test]
    fn copies_are_told_apart_by_their_source_and_completed_on_a_repeated_import() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let catalog = Library::create(&tmp.path().join("Rawtonic"), "Main", Mode::Catalog).unwrap();
        let photos = tmp.path().join("Photos");
        fs::create_dir_all(&photos).unwrap();
        let path = photos.join("A.NEF");
        fs::write(&path, b"a").unwrap();
        let library = |uid: &str| {
            let mut copy = photo("2", path.clone(), Develop::None, Some("1"));
            copy.uid = Some(uid.into());
            Source {
                name: "Lightroom Catalog".into(),
                photos: vec![photo("1", path.clone(), Develop::None, None), copy],
                albums: vec![],
                not_read: NotRead::default(),
            }
        };
        let (first, second) = (library("UUID-A"), library("UUID-B"));
        // The folder refuses new files: the copy is kept by the catalog, its
        // sidecar is not there.
        fs::set_permissions(&photos, fs::Permissions::from_mode(0o555)).unwrap();
        let refused = import_into(&catalog, &first).unwrap();
        fs::set_permissions(&photos, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(refused.errors.len(), 1, "{refused:?}");
        let asset = catalog.asset_at(&path).unwrap().unwrap();
        let id = copy_id(
            &asset.id,
            &copy_origin("Lightroom Catalog", &first.photos[1]),
            0,
        );
        assert_eq!(catalog.copies(&asset.id).unwrap(), vec![id.clone()]);
        let sidecar = photos.join(format!("A.NEF.{id}.rrdata"));
        assert!(!sidecar.exists());
        // Imported again: the same copy, now with its sidecar.
        let again = import_into(&catalog, &first).unwrap();
        assert!(again.errors.is_empty(), "{again:?}");
        assert!(sidecar.exists());
        assert_eq!(catalog.copies(&asset.id).unwrap().len(), 1);
        // Another catalog's copy with the same local key is another copy.
        import_into(&catalog, &second).unwrap();
        assert_eq!(catalog.copies(&asset.id).unwrap().len(), 2);
        assert_eq!(fs::read(&path).unwrap(), b"a");
    }

    fn condition(criteria: &str, operation: &str, value: SmartValue) -> SmartRule {
        SmartRule::Condition {
            criteria: criteria.into(),
            operation: operation.into(),
            value,
            value2: SmartValue::None,
            units: None,
        }
    }

    /// Lightroom's smart collection criteria as smart album rules; one
    /// criterion without a counterpart leaves the whole collection out.
    #[test]
    fn lightroom_smart_collections_translate() {
        let rules = |combine: &str, rules: Vec<SmartRule>| SmartRules {
            combine: combine.into(),
            rules,
        };
        let rule = smart_rule(&rules(
            "union",
            vec![
                condition("labelColor", "==", SmartValue::Number(1.0)),
                condition("keywords", "any", SmartValue::Text("Rom Bird".into())),
                SmartRule::Condition {
                    criteria: "captureTime".into(),
                    operation: "in".into(),
                    value: SmartValue::Text("2010-01-01".into()),
                    value2: SmartValue::Text("2020-12-31".into()),
                    units: None,
                },
                SmartRule::Condition {
                    criteria: "captureTime".into(),
                    operation: "inLast".into(),
                    value: SmartValue::Number(1.0),
                    value2: SmartValue::None,
                    units: Some("months".into()),
                },
                SmartRule::Group(rules(
                    "exclude",
                    vec![condition(
                        "fileFormat",
                        "==",
                        SmartValue::Text("JPG".into()),
                    )],
                )),
            ],
        ))
        .unwrap();
        assert_eq!(
            serde_json::to_value(&rule).unwrap(),
            json!({"combine": "any", "rules": [
                {"field": "label", "op": "is", "value": "red"},
                {"field": "keyword", "op": "containsAny", "value": "Rom Bird"},
                {"field": "captureDate", "op": "between", "value": ["2010-01-01", "2020-12-31"]},
                {"field": "captureDate", "op": "inLast", "value": {"amount": 1, "unit": "months"}},
                {"combine": "none", "rules": [{"field": "fileType", "op": "is", "value": "jpeg"}]},
            ]})
        );
        for unknown in [
            condition("hasAdjustments", "==", SmartValue::Bool(true)),
            condition("fileFormat", "==", SmartValue::Text("VIDEO".into())),
            condition("labelColor", "==", SmartValue::Number(6.0)),
        ] {
            assert!(smart_rule(&rules("intersect", vec![unknown])).is_none());
        }
    }

    #[test]
    fn lightroom_spots_and_red_eyes_are_imported_with_the_photos_orientation() {
        let tmp = tempfile::tempdir().unwrap();
        let catalog = Library::create(&tmp.path().join("Rawtonic"), "Main", Mode::Catalog).unwrap();
        // A JPEG decoded turned a quarter (orientation 6).
        let path = tmp.path().join("IMG_1.jpg");
        let mut jpeg = Vec::new();
        image::DynamicImage::new_rgb8(16, 8)
            .write_to(
                &mut std::io::Cursor::new(&mut jpeg),
                image::ImageFormat::Jpeg,
            )
            .unwrap();
        let mut metadata = little_exif::metadata::Metadata::new();
        metadata.set_tag(little_exif::exif_tag::ExifTag::Orientation(vec![6]));
        metadata
            .write_to_vec(&mut jpeg, little_exif::filetype::FileExtension::JPEG)
            .unwrap();
        fs::write(&path, &jpeg).unwrap();
        assert_eq!(
            crate::image_loader::source_orientation(&jpeg, "IMG_1.jpg"),
            6
        );
        let xmp = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
 <rdf:Description xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"><crs:RetouchInfo><rdf:Seq>
  <rdf:li>centerX = 0.25, centerY = 0.5, radius = 0.02, sourceState = sourceAutoComputed, sourceX = 0.4, sourceY = 0.5, spotType = heal</rdf:li>
 </rdf:Seq></crs:RetouchInfo><crs:RedEyeInfo><rdf:Seq>
  <rdf:li>x = 0.5, y = 0.5, width = 0.01, height = 0.02, alpha = 0, pupilSize = 0.5, pupilDarkenAmount = 0.5, adaptivePupilColor = 0</rdf:li>
 </rdf:Seq></crs:RedEyeInfo></rdf:Description></rdf:RDF></x:xmpmeta>"#;
        let source = Source {
            name: "LR".into(),
            photos: vec![photo(
                "1",
                path.clone(),
                Develop::LightroomXmp(xmp.into()),
                None,
            )],
            albums: vec![],
            not_read: NotRead::default(),
        };
        assert_eq!(import_into(&catalog, &source).unwrap().imported, 1);
        let asset = catalog.asset_at(&path).unwrap().unwrap();
        let state = catalog.editor_state(&asset.id).unwrap().unwrap();
        let adjustments = &state["adjustments"];
        for key in ["spots", "redEyes"] {
            assert_eq!(
                adjustments[key][0]["lrNormalized"]["orientation"], 6,
                "{key}"
            );
        }
        // Renders place them without the editor: the photo is never opened.
        assert!(!rawtonic_engine::lightroom_retouch::needs_orientation(
            adjustments
        ));
        assert!(crate::image_loader::has_base_retouch(adjustments));
        // Nothing was written beside the photograph.
        assert_eq!(fs::read(&path).unwrap(), jpeg);
    }

    #[test]
    fn lightroom_masks_are_imported_with_the_photos_orientation() {
        let tmp = tempfile::tempdir().unwrap();
        let catalog = Library::create(&tmp.path().join("Rawtonic"), "Main", Mode::Catalog).unwrap();
        let path = tmp.path().join("IMG_2.jpg");
        let mut jpeg = Vec::new();
        image::DynamicImage::new_rgb8(16, 8)
            .write_to(
                &mut std::io::Cursor::new(&mut jpeg),
                image::ImageFormat::Jpeg,
            )
            .unwrap();
        let mut metadata = little_exif::metadata::Metadata::new();
        metadata.set_tag(little_exif::exif_tag::ExifTag::Orientation(vec![8]));
        metadata
            .write_to_vec(&mut jpeg, little_exif::filetype::FileExtension::JPEG)
            .unwrap();
        fs::write(&path, &jpeg).unwrap();
        let xmp = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
 <rdf:Description xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" crs:ProcessVersion="15.4">
 <crs:MaskGroupBasedCorrections><rdf:Seq>
  <rdf:li><rdf:Description crs:CorrectionName="Sky" crs:LocalExposure2012="-0.1"><crs:CorrectionMasks><rdf:Seq>
   <rdf:li><rdf:Description crs:What="Mask/Gradient" crs:ZeroX="0.5" crs:ZeroY="0.6" crs:FullX="0.5" crs:FullY="0.2"/></rdf:li>
   <rdf:li><rdf:Description crs:What="Mask/CircularGradient" crs:MaskBlendMode="1" crs:Top="0.2" crs:Left="0.3" crs:Bottom="0.6" crs:Right="0.7" crs:Angle="10" crs:Feather="50"/></rdf:li>
  </rdf:Seq></crs:CorrectionMasks></rdf:Description></rdf:li>
  <rdf:li><rdf:Description crs:CorrectionName="Subject" crs:LocalExposure2012="0.1"><crs:CorrectionMasks><rdf:Seq>
   <rdf:li><rdf:Description crs:What="Mask/Image" crs:MaskSubType="1"/></rdf:li>
  </rdf:Seq></crs:CorrectionMasks></rdf:Description></rdf:li>
 </rdf:Seq></crs:MaskGroupBasedCorrections></rdf:Description></rdf:RDF></x:xmpmeta>"#;
        let source = Source {
            name: "LR".into(),
            photos: vec![photo(
                "1",
                path.clone(),
                Develop::LightroomXmp(xmp.into()),
                None,
            )],
            albums: vec![],
            not_read: NotRead::default(),
        };
        let report = import_into(&catalog, &source).unwrap();
        assert_eq!((report.imported, report.ai_masks_on_open), (1, 1));
        let asset = catalog.asset_at(&path).unwrap().unwrap();
        let state = catalog.editor_state(&asset.id).unwrap().unwrap();
        let masks = &state["adjustments"]["masks"];
        let subs = masks[0]["subMasks"].as_array().unwrap();
        assert_eq!(subs.len(), 2);
        for sub in subs {
            assert_eq!(sub["parameters"]["lrNormalized"]["orientation"], 8, "{sub}");
        }
        // Renders place them without the editor: the photo is never opened.
        assert!(!rawtonic_engine::lightroom_masks::needs_orientation(
            &state["adjustments"]
        ));
        assert_eq!(masks[1]["subMasks"][0]["type"], "ai-foreground");
        assert_eq!(fs::read(&path).unwrap(), jpeg);
    }

    /// Imports `$LR_IMPORT_FILE` with the Lightroom settings `$LR_IMPORT_XMP`
    /// into a new catalog, writing its edit as imported to
    /// `$LR_IMPORT_OUT-pending.json` and as the editor places it once the
    /// photo is open (its full decode's size) to `$LR_IMPORT_OUT-placed.json`.
    #[test]
    #[ignore = "tool: needs LR_IMPORT_XMP, LR_IMPORT_FILE and LR_IMPORT_OUT"]
    fn import_lightroom_edit_for_file() {
        let var = |k: &str| std::env::var(k).unwrap();
        let path = std::path::PathBuf::from(var("LR_IMPORT_FILE"));
        let tmp = tempfile::tempdir().unwrap();
        let catalog = Library::create(&tmp.path().join("Rawtonic"), "Main", Mode::Catalog).unwrap();
        let source = Source {
            name: "LR".into(),
            photos: vec![photo(
                "1",
                path.clone(),
                Develop::LightroomXmp(fs::read_to_string(var("LR_IMPORT_XMP")).unwrap()),
                None,
            )],
            albums: vec![],
            not_read: NotRead::default(),
        };
        assert_eq!(import_into(&catalog, &source).unwrap().imported, 1);
        let asset = catalog.asset_at(&path).unwrap().unwrap();
        let pending = catalog.editor_state(&asset.id).unwrap().unwrap()["adjustments"].clone();
        let bytes = fs::read(&path).unwrap();
        let name = path.to_string_lossy();
        let settings = crate::app_settings::AppSettings::default();
        let full =
            crate::image_loader::load_base_image_from_bytes(&bytes, &name, false, &settings, None)
                .unwrap();
        let mut placed = pending.clone();
        crate::preset_converter::place_lightroom_geometry(
            &mut placed,
            f64::from(full.width()),
            f64::from(full.height()),
            crate::image_loader::source_orientation(&bytes, &name),
        );
        let out = var("LR_IMPORT_OUT");
        for (suffix, recipe) in [("pending", &pending), ("placed", &placed)] {
            rawtonic_export::output::write_new_output(
                format!("{out}-{suffix}.json"),
                recipe.to_string(),
            )
            .unwrap();
        }
        println!("{}x{}", full.width(), full.height());
    }

    #[test]
    fn sessions_are_refused_as_targets() {
        let tmp = tempfile::tempdir().unwrap();
        let session = Library::create(&tmp.path().join("Job"), "Job", Mode::Session).unwrap();
        let source = Source {
            name: "x".into(),
            photos: vec![],
            albums: vec![],
            not_read: NotRead::default(),
        };
        assert!(import_into(&session, &source).is_err());
    }

    /// Imports a real library (`RAWTONIC_IMPORT_SAMPLE`) into a temporary
    /// catalog: the photographs are only read. Run by hand. With
    /// `RAWTONIC_IMPORT_OUT`, writes what landed in the catalog per photo
    /// (`<source key>.json`: path, metadata, recipe) and `albums.json` there,
    /// for field-by-field comparisons and CLI renders (`--adjustments`).
    #[test]
    #[ignore]
    fn import_a_real_library_into_a_scratch_catalog() {
        let source = std::env::var("RAWTONIC_IMPORT_SAMPLE").unwrap();
        // The lens database the app loads at start (profile corrections of
        // lenses without camera data).
        if let Ok(db) = std::env::var("RAWTONIC_LENSFUN_DB") {
            rawtonic_engine::lens_correction::set_global_lens_db(std::sync::Arc::new(
                rawtonic_engine::lens_correction::load_lensfun_dir(Path::new(&db)),
            ));
        }
        let tmp = tempfile::tempdir().unwrap();
        let catalog =
            Library::create(&tmp.path().join("Scratch"), "Scratch", Mode::Catalog).unwrap();
        let started = std::time::Instant::now();
        let (mut lib, kind) = read_source(Path::new(&source)).unwrap();
        // `FROM|TO`: the photographs' folder moved (scratch clones).
        if let Ok(relink) = std::env::var("RAWTONIC_IMPORT_RELINK")
            && let Some((from, to)) = relink.split_once('|')
        {
            lib.relink(Path::new(from), Path::new(to));
        }
        let read = started.elapsed().as_secs_f64();
        let report = import_into(&catalog, &lib).unwrap();
        println!(
            "{kind}: {report:?}; read {read:.2} s, read + import {:.2} s",
            started.elapsed().as_secs_f64()
        );
        let Ok(out) = std::env::var("RAWTONIC_IMPORT_OUT") else {
            return;
        };
        let out = Path::new(&out);
        fs::create_dir_all(out).unwrap();
        for photo in &lib.photos {
            let asset = catalog.asset_at(&photo.path).unwrap();
            let copy = asset
                .as_ref()
                .filter(|_| photo.copy_of.is_some())
                .map(|a| copy_id(&a.id, &copy_origin(&lib.name, photo), 0));
            let (metadata, state, snapshots) = match &asset {
                Some(asset) => {
                    let state = match &copy {
                        Some(copy) => catalog.copy_state(&asset.id, copy).unwrap(),
                        None => catalog.editor_state(&asset.id).unwrap(),
                    };
                    let version = catalog.version_for(&asset.id, copy.as_deref()).unwrap();
                    let snapshots = version
                        .map(|v| catalog.snapshots(&v).unwrap())
                        .unwrap_or_default();
                    (
                        serde_json::to_value(catalog.metadata(&asset.id).unwrap()).unwrap(),
                        state,
                        snapshots,
                    )
                }
                None => (Value::Null, None, Vec::new()),
            };
            let record = json!({
                "path": photo.path,
                "copyOf": photo.copy_of,
                "copy": copy,
                "asset": asset.as_ref().map(|a| a.id.clone()),
                "metadata": metadata,
                "copyRating": state.as_ref().map(|s| s["rating"].clone()),
                "copyTags": state.as_ref().map(|s| s["tags"].clone()),
                "adjustments": state.map(|s| s["adjustments"].clone()),
                "snapshots": snapshots.iter().map(|s| json!({"name": s.name, "adjustments": s.adjustments})).collect::<Vec<_>>(),
            });
            fs::write(
                out.join(format!("{}.json", photo.key)),
                serde_json::to_vec_pretty(&record).unwrap(),
            )
            .unwrap();
        }
        let albums: Vec<Value> = catalog
            .collections()
            .unwrap()
            .into_iter()
            .map(|c| {
                let members: Vec<String> = catalog
                    .collection_assets(&c.id)
                    .map(|a| a.into_iter().map(|a| a.id).collect())
                    .unwrap_or_default();
                json!({"collection": c, "members": members})
            })
            .collect();
        fs::write(
            out.join("albums.json"),
            serde_json::to_vec_pretty(&json!({"report": format!("{report:?}"), "albums": albums}))
                .unwrap(),
        )
        .unwrap();
    }
}
