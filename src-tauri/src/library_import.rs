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
//! Lightroom's folder stacks become stacks (library data only), with
//! Lightroom's top, order and collapsed state, virtual copies included.
use rawtonic_catalog::stacks::{Member, Stack};
use rawtonic_catalog::{ALBUM, Combine, Field, GROUP, Library, Mode, Op, Rule, SMART};
use rawtonic_interop::{
    Develop, Library as Source, NotRead, Photo, Root, SmartRule, SmartRules, SmartValue,
};
use rawtonic_metadata::photo_ref;
use rawtonic_recipe::geometry;
use rawtonic_recipe::lens;
use rawtonic_recipe::masks;
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
    } else if name.ends_with(".cosessiondb") || name.ends_with(".cocatalogdb") {
        rawtonic_interop::capture_one::read(path).map(|lib| (lib, "captureOne"))
    } else if name.ends_with(".cocatalog") {
        // A Capture One catalog is a folder (a package on macOS) holding
        // `<name>.cocatalogdb`.
        let mut name = path.file_stem().unwrap_or_default().to_os_string();
        name.push(".cocatalogdb");
        let db = path.join(name);
        rawtonic_interop::capture_one::read(&db).map(|lib| (lib, "captureOne"))
    } else {
        return Err(
            "Choose a Lightroom Classic catalog (.lrcat), a Capture One session (.cosessiondb) or a Capture One catalog (.cocatalog)."
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
    /// Lightroom stacks made (or completed by a repeated import).
    pub stacks: usize,
    /// Photographs with Lightroom AI masks (Select Subject, Select Sky):
    /// their model runs when the photo is opened in Develop, so exports
    /// before that leave those masks' layers without effect.
    pub ai_masks_on_open: usize,
    /// Smart collections left out, each with the criterion that has no
    /// counterpart (never imported with other rules or other members).
    pub smart_albums_skipped: Vec<String>,
    /// Albums with a virtual copy among their members: a Rawtonic album
    /// holds photographs, so it holds the copy's photograph.
    pub copies_in_albums: Vec<String>,
    /// Smart albums imported on camera, lens, ISO or capture date: the
    /// catalog fills them as the background pass reads the files, and
    /// camera and lens words are matched in the name the file states, not
    /// as Lightroom does, so they can differ from Lightroom's.
    pub smart_albums_by_facts: Vec<String>,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

fn preview(source: &Source, kind: &'static str) -> ImportPreview {
    let originals = || source.photos.iter().filter(|p| p.copy_of.is_none());
    ImportPreview {
        name: source.name.clone(),
        kind,
        photos: originals().count(),
        copies: source.photos.len() - originals().count(),
        edited: originals()
            .filter(|p| !matches!(p.develop, Develop::None) || !p.capture_one_layers.is_empty())
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

/// What a smart collection's rules depend on that the catalog reads from
/// the files after the import: `Some(note)` for camera, lens, ISO or
/// capture date. The catalog finds camera and lens words anywhere in the
/// name the file states, Lightroom whole words in names from its own
/// tables, so those members can differ.
fn smart_rule_facts_note(rules: &SmartRules) -> Option<&'static str> {
    const TEXT: &str = "filled as the files' capture data is read; camera and lens words match anywhere in the name the file states, Lightroom matches whole words in names from its own tables, so the members can differ";
    const FACTS: &str = "filled as the files' capture data is read";
    rules
        .rules
        .iter()
        .filter_map(|rule| match rule {
            SmartRule::Group(group) => smart_rule_facts_note(group),
            SmartRule::Condition { criteria, .. } => match criteria.as_str() {
                "camera" | "lens" => Some(TEXT),
                "isoSpeedRating" | "captureTime" => Some(FACTS),
                _ => None,
            },
        })
        .max_by_key(|note| note.len())
}

/// What stops a smart collection's rules from translating: the first
/// criterion (or combination) without a counterpart, named as Lightroom
/// names it.
pub fn smart_rule_gap(rules: &SmartRules) -> Option<String> {
    if !matches!(rules.combine.as_str(), "intersect" | "union" | "exclude") {
        return Some(format!("combination “{}”", rules.combine));
    }
    rules.rules.iter().find_map(|rule| match rule {
        SmartRule::Group(group) => smart_rule_gap(group),
        SmartRule::Condition {
            criteria,
            operation,
            value,
            value2,
            units,
        } => smart_condition(criteria, operation, value, value2, units.as_deref())
            .is_none()
            .then(|| format!("“{criteria}” ({operation})")),
    })
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
        // Lightroom's camera name is the maker and the model ("Canon EOS
        // 600D"); the catalog keeps them apart, so a word is looked for in
        // either. A whole name has no column to compare with.
        "camera" => {
            let in_either = |op: Op, words: &str| {
                [Field::Camera, Field::Make]
                    .into_iter()
                    .map(|field| Rule::Condition {
                        field,
                        op,
                        value: json!(words),
                    })
                    .collect::<Vec<_>>()
            };
            let either = |op: Op, words: &str| Rule::Group {
                combine: Combine::Any,
                rules: in_either(op, words),
            };
            return match operation {
                "words" | "all" => Some(Rule::Group {
                    combine: Combine::All,
                    rules: text(value)?
                        .split_whitespace()
                        .map(|word| either(Op::Contains, word))
                        .collect(),
                }),
                "any" => Some(either(Op::ContainsAny, &text(value)?)),
                "noneOf" => Some(Rule::Group {
                    combine: Combine::None,
                    rules: in_either(Op::ContainsAny, &text(value)?),
                }),
                "empty" | "notEmpty" => Some(Rule::Condition {
                    field: Field::Camera,
                    op: if operation == "empty" {
                        Op::Empty
                    } else {
                        Op::NotEmpty
                    },
                    value: Value::Null,
                }),
                _ => None,
            };
        }
        // Text of the photograph's own facts: Lightroom's word search and
        // comparisons, as the catalog's.
        "lens" | "filename" => {
            let op = match operation {
                "words" | "all" => Op::Contains,
                "any" => Op::ContainsAny,
                "noneOf" => Op::NotContains,
                "==" => Op::Is,
                "!=" => Op::IsNot,
                "beginsWith" => Op::StartsWith,
                "endsWith" => Op::EndsWith,
                "empty" => Op::Empty,
                "notEmpty" => Op::NotEmpty,
                _ => return None,
            };
            let field = if criteria == "lens" {
                Field::Lens
            } else {
                Field::Filename
            };
            let value = match op {
                Op::Empty | Op::NotEmpty => Value::Null,
                _ => json!(text(value)?),
            };
            (field, op, value)
        }
        "isoSpeedRating" if operation == "in" => (
            Field::Iso,
            Op::Between,
            json!([number(value)?, number(value2)?]),
        ),
        "isoSpeedRating" => (Field::Iso, compare(operation)?, json!(number(value)?)),
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

fn import_warnings(photo: &Photo) -> Vec<String> {
    let mut layers = match &photo.develop {
        Develop::CaptureOne(entries) => capture_one_shadows_report(entries)
            .into_iter()
            .chain(capture_one_lens_report(entries))
            .collect(),
        _ => Vec::new(),
    };
    layers.extend(crate::capture_one_layers::report(&photo.capture_one_layers));
    let layers = layers
        .into_iter()
        .map(|line| format!("{}: {line}", photo.path.display()));
    std::iter::once((None, &photo.develop))
        .chain(
            photo
                .snapshots
                .iter()
                .map(|(name, develop)| (Some(name), develop)),
        )
        .flat_map(|(snapshot, develop)| {
            let Develop::LightroomXmp(xmp) = develop else {
                return Vec::new();
            };
            let context = photo
                .copy_name
                .iter()
                .chain(snapshot)
                .map(|name| format!(" ({name})"))
                .collect::<String>();
            [
                crate::preset_converter::legacy_process_warning(xmp),
                crate::preset_converter::tone_curve_warning(xmp),
                crate::preset_converter::point_color_overflow_warning(xmp),
                crate::preset_converter::lightroom_subtract_warning(xmp).map(str::to_string),
                crate::preset_converter::skipped_masks_warning(xmp),
            ]
            .into_iter()
            .flatten()
            .map(|warning| format!("{}{}: {warning}", photo.path.display(), context))
            .collect()
        })
        .chain(layers)
        .collect()
}

/// Registers the photographs that exist, with metadata, translated edits
/// and albums, in `catalog`.
pub fn import_into(catalog: &Library, source: &Source) -> Result<ImportReport, String> {
    if catalog.manifest.mode != Mode::Catalog {
        return Err("Open a Rawtonic catalog to import into: sessions hold only photographs inside their own folder.".into());
    }
    let mut report = ImportReport::default();
    let mut assets: HashMap<&str, String> = HashMap::new();
    // Source keys → stack members here (photographs and virtual copies).
    let mut members: HashMap<&str, Member> = HashMap::new();
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
                report.warnings.extend(import_warnings(photo));
                members.insert(photo.key.as_str(), Member::photo(asset.clone()));
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
            Ok((imported, copy)) => {
                members.insert(
                    photo.key.as_str(),
                    Member {
                        asset_id: asset.clone(),
                        copy,
                    },
                );
                report.copies += 1;
                report.ai_masks_on_open += usize::from(imported.ai_masks);
                report.crops_clipped += imported.crops_clipped;
                report.warnings.extend(import_warnings(photo));
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
    report.smart_albums_skipped.extend(
        source
            .not_read
            .smart_album_names
            .iter()
            .map(|name| format!("{name}: its rules could not be read")),
    );
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
                    // Announced in the preview as not imported, named here.
                    None => {
                        report.smart_albums_skipped.push(format!(
                            "{}: no counterpart for {}",
                            album.name,
                            album
                                .smart
                                .as_ref()
                                .and_then(smart_rule_gap)
                                .unwrap_or_else(|| "its rules".into())
                        ));
                        continue;
                    }
                },
                None => None,
            };
            if smart.is_some()
                && let Some(note) = album.smart.as_ref().and_then(smart_rule_facts_note)
            {
                report
                    .smart_albums_by_facts
                    .push(format!("{}: {note}", album.name));
            }
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
                        let photo = source.photos.iter().find(|p| &p.key == key)?;
                        let member = assets
                            .get(photo.copy_of.as_deref().unwrap_or(&photo.key))
                            .cloned()?;
                        if photo.copy_of.is_some() {
                            report.copies_in_albums.push(format!(
                                "{}: {}{}",
                                album.name,
                                photo
                                    .path
                                    .file_name()
                                    .map(|n| n.to_string_lossy().into_owned())
                                    .unwrap_or_default(),
                                photo
                                    .copy_name
                                    .as_ref()
                                    .map(|n| format!(" ({n})"))
                                    .unwrap_or_default()
                            ));
                        }
                        Some(member)
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
    report.stacks = import_stacks(catalog, source, &members)?;
    Ok(report)
}

/// The source's stacks over the photographs and copies imported
/// (`members`), in one transaction (`Library::import_stacks`): each under
/// an id derived from the source's stack id, so a repeated import finds the
/// stack it made (and adds what it lacks) instead of making another; a
/// photograph in a stack here (the photographer's own, or another
/// library's) stays where it is. Returns how many stacks were made or
/// completed.
fn import_stacks(
    catalog: &Library,
    source: &Source,
    members: &HashMap<&str, Member>,
) -> Result<usize, String> {
    let stacks: Vec<Stack> = source
        .stacks
        .iter()
        .map(|stack| Stack {
            id: format!("imported-{}", stack_id(&source.name, &stack.key)),
            collapsed: stack.collapsed,
            members: stack
                .photos
                .iter()
                .filter_map(|key| members.get(key.as_str()).cloned())
                .collect(),
        })
        .filter(|stack| stack.members.len() >= 2)
        .collect();
    if stacks.is_empty() {
        return Ok(0);
    }
    let change = catalog.import_stacks(&stacks).map_err(|e| e.to_string())?;
    Ok(change.after.len())
}

/// A stable id for the source stack `key`: Lightroom's stack UUID (which a
/// copied catalog keeps) alone; a key without one (its id within the
/// catalog) together with the catalog's name.
fn stack_id(source_name: &str, key: &str) -> String {
    use sha2::Digest;
    let origin = if uuid::Uuid::parse_str(key).is_ok() {
        format!("uid\0{key}")
    } else {
        format!("key\0{source_name}\0{key}")
    };
    rawtonic_fs::hex(&sha2::Sha256::digest(origin)[..16])
}

/// Whether the translated edit holds AI masks whose data a model computes
/// when the photo is opened or exported (`ai-foreground`, `ai-sky`,
/// `ai-landscape` without data; `ai_selections::detect_missing`).
pub(crate) fn has_ai_masks_on_open(adjustments: &Value) -> bool {
    adjustments[masks::MASKS.name()]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|layer| layer[masks::SUB_MASKS.name()].as_array())
        .flatten()
        .any(|sub| {
            matches!(
                sub["type"].as_str(),
                Some("ai-foreground" | "ai-sky" | "ai-landscape")
            ) && sub["parameters"]["maskDataBase64"].is_null()
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
fn recipe_for(
    develop: &Develop,
    layers: &[rawtonic_interop::capture_one::Layer],
    path: &Path,
) -> Option<(Value, bool)> {
    recipe_for_with(
        develop,
        layers,
        path,
        &rawtonic_engine::lightroom_upright::lens_for_file,
    )
}

/// The photo's lens correction as a recipe gives it (`lens_for_file`).
type LensOf<'a> = &'a dyn Fn(
    &Value,
    &Path,
) -> Option<(
    rawtonic_engine::image_processing::WarpPoints,
    u16,
    (f64, f64),
)>;

/// `recipe_for` with the photo's lens correction from `lens`.
fn recipe_for_with(
    develop: &Develop,
    c1_layers: &[rawtonic_interop::capture_one::Layer],
    path: &Path,
    lens: LensOf,
) -> Option<(Value, bool)> {
    let translated = translate(develop);
    let mut adjustments = translated.clone().unwrap_or_else(|| json!({}));
    // Capture One's lens choice and CA switch are edits of their own: a
    // variant with only those comes over too.
    if let Develop::CaptureOne(entries) = develop {
        capture_one_lens(entries, path, &mut adjustments);
        capture_one_chromatic_aberration(entries, path, &mut adjustments);
    }
    // An HDR edit whose settings the converter does not translate is still
    // an imported edit: its HDR settings are kept (below).
    let hdr = match develop {
        Develop::LightroomXmp(xmp) => rawtonic_interop::lightroom_hdr::hdr_settings(xmp),
        _ => Default::default(),
    };
    let edited = translated.is_some()
        || !hdr.is_empty()
        || adjustments.as_object().is_some_and(|m| !m.is_empty());
    if !edited && c1_layers.is_empty() {
        return None;
    }
    let is_raw = crate::formats::is_raw_file(path);
    let mut base = json!({});
    rawtonic_engine::image_processing::apply_render_default(&mut base);
    rawtonic_engine::edge_fringe::apply_default(&mut base, true, is_raw);
    rawtonic_engine::lateral_ca::apply_default(&mut base, true, is_raw);
    // The tone sliders' and every other tool's response a new edit gets
    // (`initialAdjustmentsFor` in the editor): without the keys an import
    // rendered lowered Highlights several times darker than the other editor
    // and Clarity, noise reduction, Defringe and grain with their first
    // responses (notes: benchmark-results/hl-highlights-2026-11-01,
    // lc3-lr-import-2026-11-01).
    if let Some(map) = base.as_object_mut() {
        map.extend(rawtonic_engine::image_processing::new_edit_responses(
            is_raw,
        ));
    }
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
    // Capture One's local adjustment layers (`capture_one_layers`), above
    // any the photo's settings brought, their gradients through the lens
    // correction of the recipe as it renders (its frame stamped above).
    // Capture One's Shadow comes over as the amount that lifts as far
    // (`costyle::shadows_amount`, the unit of carried settings), the photo's
    // and its layers'; the recipe holds it as its tone version's slider value.
    if matches!(develop, Develop::CaptureOne(_)) {
        rawtonic_engine::image_processing::express_shadows_amounts(&mut adjustments);
    }
    let layers = capture_one_layers(c1_layers, path, &adjustments, lens);
    let layers = {
        let mut layered = json!({
            rawtonic_engine::image_processing::TONE_VERSION_KEY: adjustments
                .get(rawtonic_engine::image_processing::TONE_VERSION_KEY)
                .cloned()
                .unwrap_or(Value::Null),
            rawtonic_recipe::masks::MASKS.name(): layers,
        });
        rawtonic_engine::image_processing::express_shadows_amounts(&mut layered);
        match layered[rawtonic_recipe::masks::MASKS.name()].take() {
            Value::Array(layers) => layers,
            _ => Vec::new(),
        }
    };
    if !edited && layers.is_empty() {
        return None;
    }
    if !layers.is_empty()
        && let Some(map) = adjustments.as_object_mut()
    {
        let entry = map
            .entry(rawtonic_recipe::masks::MASKS.name())
            .or_insert_with(|| json!([]));
        if let Some(existing) = entry.as_array_mut() {
            existing.extend(layers);
        } else {
            *entry = Value::Array(layers);
        }
    }
    let mut clipped = false;
    if let Develop::LightroomXmp(xmp) = develop {
        // An HDR edit is not rendered (SDR only) but kept for the sidecar
        // (`rawtonic_interop::lightroom_hdr`).
        if !hdr.is_empty()
            && let Some(map) = adjustments.as_object_mut()
        {
            map.insert(
                rawtonic_interop::lightroom_hdr::LIGHTROOM_HDR_KEY.into(),
                json!(hdr),
            );
        }
        let attrs = crate::preset_converter::crs_attributes(xmp);
        lightroom_lens(&attrs, path, &mut adjustments);
        clipped = lightroom_crop(&attrs, path, &mut adjustments);
        // Guided Upright's guides, through the photo's own lens correction;
        // Lightroom's crop, when imported, frames the corrected photo.
        if crate::formats::is_raw_file(path) {
            let has_crop = adjustments
                .get(geometry::CROP.name())
                .is_some_and(|c| !c.is_null());
            let profile = attrs
                .get("LensProfileEnable")
                .is_some_and(|v| v.trim() == "1");
            rawtonic_engine::lightroom_upright::apply(
                &mut adjustments,
                xmp,
                path,
                profile,
                has_crop,
            );
        }
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
    // Capture One's raster masks are in the unrotated sensor frame: they
    // record the file's EXIF orientation that turns them into the photo's.
    if crate::capture_one_layers::needs_orientation(&adjustments)
        && let Ok(bytes) = std::fs::read(path)
    {
        let orientation = crate::image_loader::source_orientation(&bytes, &path.to_string_lossy());
        crate::capture_one_layers::set_orientation(&mut adjustments, orientation);
    }
    Some((adjustments, clipped))
}

/// Capture One's local adjustment layers as Rawtonic layers on the photo
/// at `path` whose recipe is `adjustments` (as it will render: its lens
/// choice made and its frame stamped): their gradients, drawn by Capture One
/// before its lens correction, moved through the lens correction `lens`
/// finds for that recipe (`capture_one_layers`).
fn capture_one_layers(
    layers: &[rawtonic_interop::capture_one::Layer],
    path: &Path,
    adjustments: &Value,
    lens: LensOf,
) -> Vec<Value> {
    if layers.is_empty() {
        return Vec::new();
    }
    let lens = crate::formats::is_raw_file(path)
        .then(|| lens(adjustments, path))
        .flatten();
    let through = lens.as_ref().map(|(warp, orientation, size)| {
        rawtonic_engine::lightroom_upright::through_lens(warp, *orientation, *size)
    });
    match &through {
        Some(through) => crate::capture_one_layers::layers(layers, through).0,
        None => {
            crate::capture_one_layers::layers(layers, &crate::capture_one_layers::uncorrected).0
        }
    }
}

/// The lens correction a new edit of the photo at `path` gets.
fn default_lens(path: &Path) -> Value {
    let mut default = json!({});
    rawtonic_engine::embedded_lens::apply_default(&mut default, path);
    default
}

/// A profile correction the other editor applied, on Rawtonic's own lens
/// data: left to `default` (the correction a new edit gets) where that has
/// one, else the lens's lensfun profile found from the photo's EXIF as the
/// Lens tool's auto-detect finds it (DSLR lenses). Returns whether a
/// correction applies.
fn profile_where_default_has_none(
    default: &Value,
    path: &Path,
    map: &mut serde_json::Map<String, Value>,
) -> bool {
    if default.get(lens::CORRECTION_MODE.name()).is_some() {
        return true;
    }
    let Some((maker, model, params)) = rawtonic_engine::embedded_lens::lensfun_for_photo(path)
    else {
        return false;
    };
    let Ok(params) = serde_json::to_value(params) else {
        return false;
    };
    map.insert(lens::CORRECTION_MODE.name().into(), json!("auto"));
    map.insert(lens::MAKER.name().into(), json!(maker));
    map.insert(lens::MODEL.name().into(), json!(model));
    map.insert(lens::DISTORTION_PARAMS.name().into(), params);
    true
}

/// Capture One's lens correction on Rawtonic's own lens data (Capture One's
/// profile curves in `LensGeoAbr`/`LensLightFallOff` are not used, only its
/// amounts). Its masks are drawn on the photo before that correction and
/// move with it (`capture_one_layers`), so the correction decides where they
/// land. With Distortion or Light Falloff above 0 (Capture One's defaults
/// where it has a profile: Distortion 100, Light Falloff 0) the photo gets a
/// profile correction as in `profile_where_default_has_none`, each amount
/// (0–100 %, 100 = the profile's, as Rawtonic's) as Capture One set it; with
/// both at 0 neither corrects (amounts 0, whatever the default). Settings
/// without an amount (an older session, a file without a profile) leave the
/// default.
fn capture_one_lens(entries: &[(String, String)], path: &Path, adjustments: &mut Value) {
    let Some(map) = adjustments.as_object_mut() else {
        return;
    };
    if map.contains_key(lens::CORRECTION_MODE.name()) {
        return;
    }
    let Some((distortion, falloff)) = capture_one_lens_amounts(entries) else {
        return;
    };
    let corrects = |amount: Option<f64>| amount.is_some_and(|a| a > 0.0);
    if (corrects(distortion) || corrects(falloff))
        && !profile_where_default_has_none(&default_lens(path), path, map)
    {
        return;
    }
    for (key, amount) in [
        (lens::DISTORTION_AMOUNT.name(), distortion),
        (lens::VIGNETTE_AMOUNT.name(), falloff),
    ] {
        if let Some(amount) = amount.filter(|a| *a != 100.0) {
            map.insert(key.into(), json!(amount));
        }
    }
}

/// Capture One's Distortion and Light Falloff amounts (`curve|amount|…`,
/// 0–100 %) where it has a lens profile; None without one or without
/// either amount.
fn capture_one_lens_amounts(entries: &[(String, String)]) -> Option<(Option<f64>, Option<f64>)> {
    let get = |key: &str| {
        entries
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    };
    let amount = |key: &str| {
        get(key)?
            .split('|')
            .nth(1)?
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|v| v.is_finite())
            .map(|v| v.clamp(0.0, 200.0))
    };
    get("LensIdGuid").filter(|v| !v.trim().is_empty() && !is_generic_lens(v))?;
    let amounts = (amount("LensGeoAbr"), amount("LensLightFallOff"));
    (amounts.0.is_some() || amounts.1.is_some()).then_some(amounts)
}

/// The `LensIdGuid` of Capture One's "Generic" lens profile, the one it
/// uses for a lens it has no profile of: seen the same on Olympus, Canon,
/// Leaf and DNG files of Capture One 16.6–16.9 (notes:
/// benchmark-results/c1q1-2026-11-21/). It holds no data of the lens, so
/// its Distortion and Light Falloff amounts scale a generic model, not the
/// lens's own correction.
const CAPTURE_ONE_GENERIC_LENS: &str = "E936EE15-75CA-42EC-9602-629D90D9947E";

fn is_generic_lens(guid: &str) -> bool {
    guid.trim().eq_ignore_ascii_case(CAPTURE_ONE_GENERIC_LENS)
}

/// The entry `key` of Capture One's settings.
fn capture_one_entry<'a>(entries: &'a [(String, String)], key: &str) -> Option<&'a str> {
    entries
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.trim())
}

/// The import report's line for a Capture One Shadow that lifts (or
/// darkens) further than Rawtonic's Shadows reach (`costyle::shadows_amount`).
fn capture_one_shadows_report(entries: &[(String, String)]) -> Option<String> {
    let shadow = capture_one_entry(entries, "ShadowRecovery")?
        .parse::<f64>()
        .ok()
        .filter(|v| crate::costyle::shadows_beyond_reach(*v))?;
    let end = if shadow > 0.0 { "+100" } else { "-100" };
    Some(format!(
        "Capture One's Shadow {shadow:+} reaches further than Rawtonic's Shadows: set to {end}"
    ))
}

/// The import report's lines for Capture One's lens settings Rawtonic does
/// not take over as they are: the Generic profile (the photo keeps
/// Rawtonic's default correction for its lens, never a lens profile picked
/// in its place) with the amounts set on it, which have no measured
/// equivalent on Rawtonic's sliders; and an analysed chromatic aberration
/// curve (Rawtonic measures the photo's own CA instead).
fn capture_one_lens_report(entries: &[(String, String)]) -> Vec<String> {
    let mut out = Vec::new();
    if capture_one_entry(entries, "LensIdGuid").is_some_and(is_generic_lens) {
        out.push(
            "Capture One's Generic lens profile: Rawtonic's default lens correction for this lens applies"
                .to_string(),
        );
        for (key, name) in [
            ("LensGeoAbr", "Distortion"),
            ("LensLightFallOff", "Light Falloff"),
        ] {
            let amount = capture_one_entry(entries, key)
                .and_then(|v| v.split('|').nth(1))
                .and_then(|v| v.trim().parse::<f64>().ok())
                .filter(|v| v.is_finite() && *v != 0.0);
            if let Some(amount) = amount {
                out.push(format!(
                    "Capture One's {name} {amount} with the Generic profile not carried (no measured equivalent)"
                ));
            }
        }
    }
    if capture_one_entry(entries, "LensChromAbrCustomCurve").is_some_and(|v| !v.is_empty()) {
        out.push(
            "Capture One's analysed chromatic aberration curve not carried (Rawtonic measures the photo's own)"
                .to_string(),
        );
    }
    out
}

/// Capture One's Chromatic Aberration switch as Rawtonic's CA correction:
/// on is the correction on (a RAW's new edit has it on by default, the
/// automatic correction of `lateral_ca`; other files get its switch on);
/// off turns off both the automatic correction and the lens data's own.
/// Not set (Capture One's default left alone) keeps Rawtonic's default.
fn capture_one_chromatic_aberration(
    entries: &[(String, String)],
    path: &Path,
    adjustments: &mut Value,
) {
    let Some(map) = adjustments.as_object_mut() else {
        return;
    };
    match capture_one_entry(entries, "ChromaticAberEnabled") {
        Some("0") => {
            map.insert(lens::AUTO_CHROMATIC_ABERRATION.name().into(), json!(false));
            map.insert(lens::TCA_ENABLED.name().into(), json!(false));
        }
        Some("1") if !crate::formats::is_raw_file(path) => {
            map.insert(lens::AUTO_CHROMATIC_ABERRATION.name().into(), json!(true));
        }
        _ => {}
    }
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
    if map.contains_key(lens::CORRECTION_MODE.name()) {
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
            (lens::DISTORTION_AMOUNT.name(), "LensProfileDistortionScale"),
            (lens::VIGNETTE_AMOUNT.name(), "LensProfileVignettingScale"),
        ] {
            let value = amount(lr);
            if value != 100.0 {
                map.insert(key.into(), json!(value));
            }
        }
    }
    // Which correction would the default be?
    let default = default_lens(path);
    if on {
        profile_where_default_has_none(&default, path, map);
        return;
    }
    match default[lens::CORRECTION_MODE.name()].as_str() {
        Some(rawtonic_engine::embedded_lens::CAMERA_MODE) => {
            if let Some(default) = default.as_object() {
                for (key, value) in default {
                    if key.starts_with("lens") || key.starts_with("camera") {
                        map.insert(key.clone(), value.clone());
                    }
                }
            }
            map.insert(lens::VIGNETTE_ENABLED.name().into(), json!(false));
        }
        Some(_) => {
            map.insert(lens::CORRECTION_MODE.name().into(), json!("manual"));
            map.insert(lens::DISTORTION_PARAMS.name().into(), Value::Null);
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
    map.insert(geometry::CROP.name().into(), placed.crop);
    map.insert(geometry::ROTATION.name().into(), json!(placed.rotation));
    map.insert(
        geometry::ASPECT_RATIO.name().into(),
        json!(placed.aspect_ratio),
    );
    placed.clipped
}

/// Registers one photograph with its translated edit and snapshots.
fn import_photo(catalog: &Library, photo: &Photo) -> anyhow::Result<(String, Imported)> {
    let recipe = recipe_for(&photo.develop, &photo.capture_one_layers, &photo.path);
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
        capture: None,
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
            recipe_for(develop, &[], &photo.path).unwrap_or((json!({}), false));
        clipped += usize::from(was_clipped);
        catalog.add_snapshot(version, name, adjustments)?;
    }
    Ok(clipped)
}

/// The id the virtual copy with the source identity `origin` (the other
/// editor's global id for it: Lightroom's image UUID, Capture One's session
/// file and variant id) of the asset `asset` gets: derived from both, so a
/// repeated import of that library (or, for Lightroom, of a copy of it)
/// finds the copy it made, and another library's copy of the same photo
/// gets another id (two origins share one of the 2^24 ids of an asset
/// only by a hash collision; `attempt` > 0 when that name is taken by a
/// file the catalog does not know).
fn copy_id(asset: &str, origin: &str, attempt: u32) -> String {
    use sha2::Digest;
    let digest = sha2::Sha256::digest(format!("imported-copy\0{asset}\0{origin}\0{attempt}"));
    rawtonic_fs::hex(&digest[..3])
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
) -> anyhow::Result<(Imported, String)> {
    let known = catalog.copies(asset)?;
    let origin = copy_origin(source_name, photo);
    let sidecar_of = |id: &str| {
        photo
            .path
            .with_file_name(photo_ref::sidecar_name(&photo.path, Some(id)))
    };
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
            return Ok((Imported::default(), candidate));
        }
        if !sidecar_of(&candidate).exists() {
            id = Some(candidate);
            break;
        }
    }
    let id = id.ok_or_else(|| anyhow::anyhow!("No free name for the virtual copy"))?;
    let recipe = recipe_for(&photo.develop, &photo.capture_one_layers, &photo.path);
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
    Ok((imported, id))
}

/// Writes `bytes` as the new file `path`: a synced temporary file published
/// under that name only if nothing is there.
fn publish_new(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    use std::io::Write;
    let mut file = rawtonic_fs::temp::file_for(path)?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    rawtonic_fs::no_clobber::persist(file, path)?;
    if let Some(parent) = path.parent() {
        rawtonic_fs::sync_dir(parent)?;
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
    use rusqlite::Connection;
    use std::fs;

    /// Capture One's gradients go through the lens correction of the recipe
    /// as it renders: with its frame version stamped, so frame-dependent lens
    /// data (an in-camera aspect frame) is the one preview and export use.
    #[test]
    fn capture_one_gradients_go_through_the_lens_of_the_final_recipe() {
        use rawtonic_engine::image_processing::{GeometryParams, WarpPoints};
        use rawtonic_interop::capture_one::Layer;
        use rawtonic_interop::capture_one_masks::Mask;
        let layer = Layer {
            name: "Sky".into(),
            enabled: true,
            opacity: 100.0,
            settings: vec![("Exposure".into(), "-1".into())],
            mask: Mask::Linear {
                start: (100.0, 50.0),
                end: (100.0, 150.0),
                midpoint: 0.5,
            },
            frame: (600.0, 400.0),
        };
        let seen = std::cell::RefCell::new(Vec::new());
        let lens = |recipe: &Value, _: &Path| {
            seen.borrow_mut().push(recipe.clone());
            let barrel = GeometryParams {
                distortion: 40.0,
                ..GeometryParams::default()
            };
            Some((WarpPoints::new(&barrel, 600, 400), 1, (600.0, 400.0)))
        };
        let develop = Develop::CaptureOne(vec![("Exposure".into(), "0.3".into())]);
        let path = Path::new("missing/a.NEF");
        let (recipe, _) =
            recipe_for_with(&develop, std::slice::from_ref(&layer), path, &lens).unwrap();
        let seen = seen.into_inner();
        assert_eq!(seen.len(), 1);
        assert_eq!(
            seen[0][rawtonic_engine::source_frame::KEY],
            json!(rawtonic_engine::source_frame::VERSION)
        );
        // Moved by the correction, not left where Capture One drew it.
        let (unmoved, _) =
            recipe_for_with(&develop, &[layer], path, &|_: &Value, _: &Path| None).unwrap();
        let g = |r: &Value| r["masks"][0]["subMasks"][0]["parameters"]["lrNormalized"].clone();
        assert!((g(&unmoved)["fullY"].as_f64().unwrap() - 0.125).abs() < 1e-9);
        assert!((g(&recipe)["fullY"].as_f64().unwrap() - 0.125).abs() > 1e-4);
    }

    /// Capture One's lens choice: its amounts, never its profile curves; a
    /// photo whose lens Rawtonic has no profile for keeps the default.
    #[test]
    fn capture_one_lens_amounts_come_over_without_its_curves() {
        let entries = |distortion: &str, falloff: &str, guid: &str| -> Vec<(String, String)> {
            [
                ("LensGeoAbr", distortion),
                ("LensLightFallOff", falloff),
                ("LensIdGuid", guid),
            ]
            .iter()
            .filter(|(_, v)| !v.is_empty())
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
        };
        let curve = "0,0;1000,-10;2000,-40";
        let on = entries(
            &format!("{curve}|100.0|1|100|-1.0"),
            "1000,-0.01|0.0|1",
            "G",
        );
        assert_eq!(
            capture_one_lens_amounts(&on),
            Some((Some(100.0), Some(0.0)))
        );
        let half = entries(&format!("{curve}|50.0|1|100|-1.0"), "", "G");
        assert_eq!(capture_one_lens_amounts(&half), Some((Some(50.0), None)));
        let falloff_only = entries("", "1000,-0.01|40.0|1", "G");
        assert_eq!(
            capture_one_lens_amounts(&falloff_only),
            Some((None, Some(40.0)))
        );
        // No profile in Capture One, or no amount: nothing to bring over.
        assert_eq!(
            capture_one_lens_amounts(&entries(&format!("{curve}|100.0|1"), "", "")),
            None
        );
        assert_eq!(capture_one_lens_amounts(&entries("", "", "G")), None);
        assert_eq!(capture_one_lens_amounts(&entries("garbage", "", "G")), None);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.NEF");
        fs::write(&path, b"not a raw").unwrap();
        // Both amounts 0: the photo is neither moved nor brightened,
        // whatever the default.
        let off = entries(&format!("{curve}|0.0|1|100|-1.0"), "1000,-0.01|0.0|1", "G");
        let mut adjustments = json!({});
        capture_one_lens(&off, &path, &mut adjustments);
        assert_eq!(
            adjustments,
            json!({"lensDistortionAmount": 0.0, "lensVignetteAmount": 0.0})
        );
        // On, but no profile for this lens: the default (nothing) stays.
        for on in [&on, &falloff_only] {
            let mut adjustments = json!({});
            capture_one_lens(on, &path, &mut adjustments);
            assert_eq!(adjustments, json!({}));
        }
        // A lens mode set already is kept.
        let mut adjustments = json!({"lensCorrectionMode": "manual"});
        capture_one_lens(&off, &path, &mut adjustments);
        assert_eq!(adjustments, json!({"lensCorrectionMode": "manual"}));
    }

    /// C1Q1: Capture One's Generic lens profile (no profile of the lens) is
    /// not swapped for a lens profile: the photo keeps Rawtonic's default
    /// for its lens, and the report says so and names Generic amounts that
    /// have no measured equivalent. Chromatic Aberration on/off is carried
    /// as Rawtonic's CA correction on/off.
    #[test]
    fn capture_one_generic_lens_and_chromatic_aberration() {
        let entries = |pairs: &[(&str, &str)]| -> Vec<(String, String)> {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        };
        let generic = entries(&[
            ("LensIdGuid", CAPTURE_ONE_GENERIC_LENS),
            ("LensGeoAbr", "0,0;1000,-10;2000,-40|30.0|1|100|-1"),
            ("LensLightFallOff", "|0.0|1"),
            ("ChromaticAberEnabled", "1"),
        ]);
        let dir = tempfile::tempdir().unwrap();
        let raw = dir.path().join("a.ORF");
        fs::write(&raw, b"not a raw").unwrap();
        // No lens profile in Capture One: no amounts to put on one.
        assert_eq!(capture_one_lens_amounts(&generic), None);
        let mut adjustments = json!({});
        capture_one_lens(&generic, &raw, &mut adjustments);
        assert_eq!(adjustments, json!({}), "the default lens correction stays");
        assert_eq!(
            capture_one_lens_report(&generic),
            [
                "Capture One's Generic lens profile: Rawtonic's default lens correction for this lens applies",
                "Capture One's Distortion 30 with the Generic profile not carried (no measured equivalent)",
            ]
        );
        let warnings =
            import_warnings(&photo("1", raw.clone(), Develop::CaptureOne(generic), None));
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(warnings[0].starts_with(&raw.display().to_string()));
        // A Generic profile left at 0 is said once, with nothing left out.
        let untouched = entries(&[
            ("LensIdGuid", CAPTURE_ONE_GENERIC_LENS),
            ("LensGeoAbr", "0,0;1000,-10|0.000000|1|100|-1"),
            ("LensLightFallOff", "|0|1"),
        ]);
        assert_eq!(capture_one_lens_report(&untouched).len(), 1);
        // A lens profile of Capture One's own reports nothing.
        let profiled = entries(&[
            ("LensIdGuid", "P"),
            ("LensGeoAbr", "0,0;1000,-10|60.0|1|100|-1"),
            ("ChromaticAberEnabled", "1"),
        ]);
        assert!(capture_one_lens_report(&profiled).is_empty());

        // Chromatic Aberration: on is Rawtonic's CA correction on (by default
        // on a RAW's new edit, set on other files), off turns it off.
        let tiff = dir.path().join("a.tif");
        let ca = |on: &str, path: &Path| {
            let mut adjustments = json!({});
            capture_one_chromatic_aberration(
                &entries(&[("ChromaticAberEnabled", on)]),
                path,
                &mut adjustments,
            );
            adjustments
        };
        assert_eq!(ca("1", &raw), json!({}));
        assert_eq!(ca("1", &tiff), json!({"autoChromaticAberration": true}));
        let off = json!({"autoChromaticAberration": false, "lensTcaEnabled": false});
        assert_eq!(ca("0", &raw), off);
        assert_eq!(ca("0", &tiff), off);
        let mut adjustments = json!({});
        capture_one_chromatic_aberration(&entries(&[]), &raw, &mut adjustments);
        assert_eq!(
            adjustments,
            json!({}),
            "not set in Capture One: the default"
        );
        // Capture One's analysed CA curve has no Rawtonic counterpart.
        let analysed = entries(&[
            ("ChromaticAberEnabled", "1"),
            ("LensChromAbrCustomCurve", "0,0;1000,1|0,0;1000,-1"),
        ]);
        assert_eq!(
            capture_one_lens_report(&analysed),
            [
                "Capture One's analysed chromatic aberration curve not carried (Rawtonic measures the photo's own)"
            ]
        );
        // Through the whole recipe: a RAW with CA off renders without it,
        // also when that is the variant's only edit.
        for develop in [
            entries(&[("Exposure", "0.5"), ("ChromaticAberEnabled", "0")]),
            entries(&[("ChromaticAberEnabled", "0")]),
        ] {
            let (recipe, _) = recipe_for(&Develop::CaptureOne(develop), &[], &raw).unwrap();
            assert_eq!(recipe["autoChromaticAberration"], false);
            assert_eq!(recipe["lensTcaEnabled"], false);
        }
        // CA on is Rawtonic's default: nothing to import.
        let on = entries(&[("ChromaticAberEnabled", "1")]);
        assert!(recipe_for(&Develop::CaptureOne(on), &[], &raw).is_none());
    }

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
            capture_one_layers: Vec::new(),
        }
    }

    /// A Capture One session named `<folder>/<name>` (its `.cosessiondb` is
    /// `<folder>/<name>/<name>.cosessiondb`), built with the tables and columns
    /// of a real Capture One 16 session: two variants of `Capture/DSCF0001.RAF`
    /// (the second modified), one trashed image, one image in a folder outside
    /// the session, one album, one smart album and one local adjustment layer.
    fn capture_one_session(dir: &Path, name: &str) -> std::path::PathBuf {
        let session = dir.join(name);
        fs::create_dir_all(session.join("Capture")).unwrap();
        fs::write(session.join("Capture/DSCF0001.RAF"), b"raf").unwrap();
        let db = session.join(format!("{name}.cosessiondb"));
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE ZENTITIES (Z_ENT INTEGER, ZNAME VARCHAR);
             INSERT INTO ZENTITIES VALUES(2,'AlbumCollection'),(5,'SmartCollection'),(7,'ProjectCollection'),
               (16,'VariantLayer'),(20,'VariantCombinedLayer'),(23,'LocalAdjustmentsLayer');
             CREATE TABLE ZPATHLOCATION (Z_ENT INTEGER, Z_PK INTEGER PRIMARY KEY, ZWINROOT VARCHAR, ZMACROOT VARCHAR,
               ZRELATIVEPATH VARCHAR, ZISRELATIVE BOOLEAN, ZVOLUME VARCHAR);
             INSERT INTO ZPATHLOCATION VALUES(38,1,NULL,NULL,'Capture',1,NULL),(38,2,NULL,'/','nowhere/else',0,'/');
             CREATE TABLE ZIMAGE (Z_ENT INTEGER, Z_PK INTEGER PRIMARY KEY, ZIMAGELOCATION INTEGER,
               ZIMAGEFILENAME VARCHAR, ZISTRASHED BOOLEAN);
             INSERT INTO ZIMAGE VALUES(12,1,1,'DSCF0001.RAF',0),(12,2,2,'DSCF0002.RAF',0),(12,3,1,'DSCF0003.RAF',1),
               (12,4,99,'DSCF0004.RAF',0);
             CREATE TABLE ZVARIANT (Z_ENT INTEGER, Z_PK INTEGER PRIMARY KEY, ZINDEX INTEGER, ZIMAGE INTEGER,
               ZCOMBINEDSETTINGS INTEGER, ZISMODIFIED BOOLEAN);
             INSERT INTO ZVARIANT VALUES(15,10,0,1,100,1),(15,11,1,1,101,0),(15,12,0,2,NULL,0),(15,13,0,3,NULL,0),
               (15,14,0,4,NULL,0);
             CREATE TABLE ZVARIANTMETADATA (Z_ENT INTEGER, Z_PK INTEGER PRIMARY KEY, ZLAYER INTEGER, ZBASIC_RATING INTEGER,
               ZCOLOR_TAG_INDEX INTEGER, ZCONTENT_KEYWORDS VARCHAR, ZSTATUS_TITLE VARCHAR, ZCONTENT_DESCRIPTION VARCHAR,
               ZCONTACT_CREATOR VARCHAR, ZSTATUS_COPYRIGHTNOTICE VARCHAR,
               ZCONTENT_HEADLINE VARCHAR, ZIMAGE_LOCATION VARCHAR, ZIMAGE_CITY VARCHAR, ZIMAGE_STATE VARCHAR,
               ZIMAGE_COUNTRY VARCHAR, ZIMAGE_ISOCOUNTRYCODE VARCHAR, ZSTATUS_JOBIDENTIFIER VARCHAR, ZSTATUS_INSTRUCTIONS VARCHAR,
               ZSTATUS_PROVIDER VARCHAR, ZSTATUS_SOURCE VARCHAR, ZSTATUS_USAGETERMS VARCHAR, ZCONTACT_CREATORSTITLE VARCHAR,
               ZCONTACT_EMAILS VARCHAR, ZCONTACT_WEBSITES VARCHAR, ZCONTACT_PHONES VARCHAR, ZCONTACT_ADDRESS VARCHAR,
               ZCONTACT_CITY VARCHAR, ZCONTACT_STATE_PROVINCE VARCHAR, ZCONTACT_POSTALCODE VARCHAR, ZCONTACT_COUNTRY VARCHAR);
             INSERT INTO ZVARIANTMETADATA (Z_ENT, Z_PK, ZLAYER, ZBASIC_RATING, ZCOLOR_TAG_INDEX, ZCONTENT_KEYWORDS,
               ZSTATUS_TITLE, ZCONTENT_DESCRIPTION, ZCONTACT_CREATOR, ZSTATUS_COPYRIGHTNOTICE, ZIMAGE_CITY, ZIMAGE_COUNTRY,
               ZCONTACT_EMAILS)
               VALUES(13,1,100,4,1,'beach, sunset','Sun','Evening','Alex Example','(c) A','Ascona','Switzerland','alex@example.org'),
                     (13,2,101,2,2,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL);
             CREATE TABLE ZCOLLECTION (Z_ENT INTEGER, Z_PK INTEGER PRIMARY KEY, ZNAME VARCHAR, ZPARENT INTEGER,
               ZCOLLECTIONINDEX INTEGER, ZSMARTCRITERIA VARCHAR);
             INSERT INTO ZCOLLECTION VALUES(7,1,'root',NULL,0,NULL),(2,6,'Picks',1,0,NULL),(5,7,'Five stars',1,1,'x'),
               (5,8,'All Images',1,2,NULL);
             CREATE TABLE ZIMAGEINCOLLECTION (Z_ENT INTEGER, Z_PK INTEGER PRIMARY KEY, ZCOLLECTION INTEGER, ZIMAGE INTEGER);
             INSERT INTO ZIMAGEINCOLLECTION VALUES(47,1,6,1);
             CREATE TABLE ZVARIANTINCOLLECTION (Z_ENT INTEGER, Z_PK INTEGER PRIMARY KEY, ZCOLLECTION INTEGER, ZVARIANT INTEGER);",
        )
        .unwrap();
        let columns: Vec<String> = [
            "ZEXPOSURE",
            "ZCONTRAST",
            "ZBRIGHTNESS",
            "ZSATURATION",
            "ZHIGHLIGHTRECOVERY",
            "ZSHADOWRECOVERY",
            "ZWHITERECOVERY",
            "ZBLACKRECOVERY",
            "ZDEHAZEAMOUNT",
            "ZCLARITY",
            "ZCLARITYSTRUCTURE",
            "ZCOLORBALANCE",
            "ZCOLORBALANCESHADOW",
            "ZCOLORBALANCEMIDTONE",
            "ZCOLORBALANCEHIGHLIGHT",
            "ZGRADATIONCURVE",
            "ZGRADATIONCURVEY",
            "ZLEVELSHIGHLIGHT",
            "ZLEVELSSHADOW",
            "ZMIDTONE",
            "ZLEVELSTARGETHIGHLIGHT",
            "ZLEVELSTARGETSHADOW",
            "ZUSMAMOUNT",
            "ZUSMRADIUS",
            "ZFILMGRAINAMOUNT",
            "ZFILMGRAINGRANULARITY",
            "ZBWENABLED",
            "ZLENSPURPLEFRINGINGAMOUNT",
            "ZMOIRE",
            "ZLENSVIGNETTING",
        ]
        .iter()
        .map(|c| c.to_string())
        .collect();
        conn.execute_batch(&format!(
            "CREATE TABLE ZVARIANTLAYER (Z_ENT INTEGER, Z_PK INTEGER PRIMARY KEY, ZVARIANT INTEGER, {});
             INSERT INTO ZVARIANTLAYER (Z_ENT, Z_PK, ZVARIANT, ZEXPOSURE, ZHIGHLIGHTRECOVERY, ZCOLORBALANCE)
               VALUES(20,100,10,0.5,-100.0,'1;0.9;0.9'),(20,101,11,-0.3,0.0,NULL),(23,102,10,NULL,NULL,NULL);",
            columns.iter().map(|c| format!("{c} VARCHAR")).collect::<Vec<_>>().join(", ")
        ))
        .unwrap();
        db
    }

    /// Two Capture One sessions with the same file name in different folders
    /// import as two origins: the same asset's virtual copy from each gets
    /// its own copy id (the session path is part of the uid the id is
    /// derived from), while a repeated import of one session finds the copy
    /// it made.
    #[test]
    fn capture_one_copies_from_two_sessions_with_one_file_name_get_their_own_ids() {
        let tmp = tempfile::tempdir().unwrap();
        let catalog = Library::create(&tmp.path().join("Rawtonic"), "Main", Mode::Catalog).unwrap();
        let root = tmp.path();
        let first = read_source(&capture_one_session(&root.join("Trip A"), "Trip"))
            .unwrap()
            .0;
        let mut second = read_source(&capture_one_session(&root.join("Trip B"), "Trip"))
            .unwrap()
            .0;
        // Two sessions reference one photograph (an external folder, or a
        // session copied aside): B's photographs are relinked to A's, so
        // both sources carry copies of one asset. (Without that, the two
        // sessions hold two different files and import as two assets.)
        assert!(
            second.relink(
                &root.join("Trip B/Trip/Capture"),
                &root.join("Trip A/Trip/Capture")
            ) >= 2
        );
        // Both sources carry variant 11 of `Capture/DSCF0001.RAF` as a copy
        // of variant 10: one local key and one asset, two origins.
        let (first_uid, second_uid) = (
            first.photos[1].uid.clone().unwrap(),
            second.photos[1].uid.clone().unwrap(),
        );
        assert_ne!(first_uid, second_uid);
        let (first_origin, second_origin) = (
            copy_origin(&first.name, &first.photos[1]),
            copy_origin(&second.name, &second.photos[1]),
        );
        assert_ne!(first_origin, second_origin);

        import_into(&catalog, &first).unwrap();
        let asset = catalog.asset_at(&first.photos[0].path).unwrap().unwrap();
        let one = catalog.copies(&asset.id).unwrap();
        assert_eq!(one, vec![copy_id(&asset.id, &first_origin, 0)]);

        // A repeated import of the same session finds that copy (its derived
        // sidecar beside the photograph), not another beside it.
        let again = import_into(&catalog, &first).unwrap();
        assert_eq!((again.copies, again.errors.len()), (1, 0), "{again:?}");
        assert_eq!(catalog.copies(&asset.id).unwrap(), one);

        // The other session's copy of that asset gets its own id beside it.
        import_into(&catalog, &second).unwrap();
        let two = copy_id(&asset.id, &second_origin, 0);
        assert_ne!(two, one[0]);
        let mut both = catalog.copies(&asset.id).unwrap();
        both.sort();
        let mut expected = vec![one[0].clone(), two.clone()];
        expected.sort();
        assert_eq!(
            both, expected,
            "session B's copy id is not derived from its origin alone"
        );
        // Neither import dropped the other's copy: a repeat of either finds
        // its own copy and adds none.
        for source in [&first, &second] {
            let again = import_into(&catalog, source).unwrap();
            assert_eq!(again.errors.len(), 0, "{again:?}");
        }
        let mut after = catalog.copies(&asset.id).unwrap();
        after.sort();
        assert_eq!(after, expected);
    }

    #[test]
    fn old_process_import_reports_each_photo_copy_and_snapshot() {
        let tmp = tempfile::tempdir().unwrap();
        let catalog = Library::create(&tmp.path().join("Library"), "Main", Mode::Catalog).unwrap();
        let path = tmp.path().join("A.png");
        image::RgbImage::new(100, 80).save(&path).unwrap();
        let original_bytes = fs::read(&path).unwrap();
        let develop = Develop::LightroomXmp(rawtonic_interop::lightroom_settings::to_xmp(
            r#"s = { ProcessVersion = "5.7", Exposure = 1.25, Exposure2012 = 0, FillLight = 10, HasCrop = true, CropLeft = 0.1, CropTop = 0.2, CropRight = 0.9, CropBottom = 0.8, CropAngle = 0 }"#
        ).unwrap());
        let mut original = photo("1", path.clone(), develop.clone(), None);
        original.snapshots.push(("Earlier".into(), develop.clone()));
        let mut copy = photo("2", path, develop.clone(), Some("1"));
        copy.snapshots.push(("Earlier".into(), develop));
        copy.copy_name = Some("Alternate".into());
        let source = Source {
            name: "Legacy".into(),
            photos: vec![original, copy],
            albums: vec![],
            stacks: vec![],
            not_read: Default::default(),
        };
        let report = import_into(&catalog, &source).unwrap();
        assert!(report.errors.is_empty(), "{report:?}");
        assert_eq!(report.warnings.len(), 4);
        assert!(
            report.warnings[0]
                .contains("A.png: Lightroom process before 2012: could not convert FillLight")
        );
        assert!(report.warnings[1].contains("(Earlier)"));
        assert!(report.warnings[2].contains("(Alternate)"));
        assert!(report.warnings[3].contains("(Alternate) (Earlier)"));
        assert_eq!(report.imported, 1);
        assert_eq!(report.copies, 1);
        assert_eq!(
            fs::read(source.photos[0].path.clone()).unwrap(),
            original_bytes
        );
        let asset = catalog.asset_at(&source.photos[0].path).unwrap().unwrap();
        let state = catalog.editor_state(&asset.id).unwrap().unwrap();
        assert_eq!(state["adjustments"]["exposure"], 1.25);
        assert_eq!(
            state["adjustments"]["crop"],
            json!({"unit": "px", "x": 10.0, "y": 16.0, "width": 80.0, "height": 48.0})
        );
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

    /// Capture One's Shadow comes over as the Shadows value that lifts the
    /// darkest quarter as far (notes: benchmark-results/shg4-2026-11-22), in
    /// the new edit's tone response, the photo's and its layers'; beyond
    /// Rawtonic's reach it is +100 with a report line. Lightroom's Shadows
    /// stays the same value (tone version 21 moves at Camera Raw's pace).
    #[test]
    fn capture_one_shadow_lifts_as_far_and_lightroom_shadows_stays() {
        let none = |_: &Value, _: &Path| None;
        let path = Path::new("missing/a.NEF");
        let c1 = |shadow: &str| {
            recipe_for_with(
                &Develop::CaptureOne(vec![("ShadowRecovery".into(), shadow.into())]),
                &[],
                path,
                &none,
            )
            .unwrap()
            .0
        };
        let tone = rawtonic_engine::recipe_versions::ToneVersion::of(
            &json!({"toneVersion": rawtonic_engine::image_processing::TONE_VERSION}),
        );
        for (shadow, amount) in [("20", 59.0), ("-30", -48.6), ("50", 92.6)] {
            let recipe = c1(shadow);
            assert_eq!(
                recipe["toneVersion"].as_u64(),
                Some(rawtonic_engine::image_processing::TONE_VERSION)
            );
            let value = recipe["shadows"].as_f64().unwrap();
            assert!(
                (tone.shadows_amount(value) - amount).abs() < 1e-6,
                "{shadow}: {value}"
            );
        }
        // Capture One's +20 is about Rawtonic's +48 (version 20's +59).
        assert!((c1("20")["shadows"].as_f64().unwrap() - 47.9).abs() < 0.1);
        assert_eq!(c1("80")["shadows"], json!(100.0));
        assert_eq!(c1("-60")["shadows"], json!(-100.0));
        let report = |shadow: &str| {
            capture_one_shadows_report(&[("ShadowRecovery".to_string(), shadow.to_string())])
        };
        assert!(report("80").is_some_and(|line| line.contains("+80") && line.contains("+100")));
        assert!(report("-60").is_some_and(|line| line.contains("-100")));
        assert!(report("60").is_none() && report("-40").is_none());
        let lr = recipe_for_with(
            &Develop::LightroomXmp(
                r#"<rdf:Description xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" crs:Shadows2012="+50"/>"#.into(),
            ),
            &[],
            path,
            &none,
        )
        .unwrap()
        .0;
        assert_eq!(lr["shadows"].as_f64(), Some(50.0));
        // A Capture One layer's Shadow, also on a photo without develop
        // settings of its own: the same value as on the photo.
        use rawtonic_interop::capture_one::Layer;
        use rawtonic_interop::capture_one_masks::Mask;
        let layer = |shadow: &str| Layer {
            name: "Shadow".into(),
            enabled: true,
            opacity: 100.0,
            settings: vec![("ShadowRecovery".into(), shadow.into())],
            mask: Mask::Linear {
                start: (100.0, 50.0),
                end: (100.0, 150.0),
                midpoint: 0.5,
            },
            frame: (600.0, 400.0),
        };
        for develop in [
            Develop::None,
            Develop::CaptureOne(vec![("ShadowRecovery".into(), "20".into())]),
        ] {
            let (recipe, _) = recipe_for_with(&develop, &[layer("20")], path, &none).unwrap();
            let value = recipe["masks"][0]["adjustments"]["shadows"]
                .as_f64()
                .unwrap();
            assert!((tone.shadows_amount(value) - 59.0).abs() < 1e-6, "{value}");
            if let Some(photo) = recipe["shadows"].as_f64() {
                assert!((tone.shadows_amount(photo) - 59.0).abs() < 1e-6, "{photo}");
            }
        }
    }

    /// A Lightroom HDR edit keeps its `crs:HDR…`/`crs:SDR…` settings in the
    /// recipe (they are written back to the sidecar, `rawtonic_metadata`);
    /// a recipe of a photo without them has no such key.
    #[test]
    fn lightroom_hdr_settings_are_kept_with_the_recipe_and_absent_otherwise() {
        let hdr = r#"s = { Exposure2012 = 0.5, HDREditMode = 1, HDRMaxValue = 4, SDRBrightness = 12, SDRBlend = 0.5 }"#;
        let xmp = rawtonic_interop::lightroom_settings::to_xmp(hdr).unwrap();
        let path = Path::new("A.NEF");
        let (adjustments, _) =
            recipe_for(&Develop::LightroomXmp(xmp), &[], path).expect("translated");
        let kept = &adjustments[rawtonic_interop::lightroom_hdr::LIGHTROOM_HDR_KEY];
        assert_eq!(kept["HDREditMode"], "1");
        assert_eq!(kept["HDRMaxValue"], "4");
        assert_eq!(kept["SDRBrightness"], "12");
        assert_eq!(adjustments["exposure"], 0.5);

        // Settings the converter has nothing to translate keep the HDR edit.
        let only = rawtonic_interop::lightroom_settings::to_xmp("s = { HDREditMode = 1 }").unwrap();
        let (adjustments, _) =
            recipe_for(&Develop::LightroomXmp(only), &[], path).expect("kept as an edit");
        assert_eq!(
            adjustments[rawtonic_interop::lightroom_hdr::LIGHTROOM_HDR_KEY]["HDREditMode"],
            "1"
        );

        let plain =
            rawtonic_interop::lightroom_settings::to_xmp("s = { Exposure2012 = 0.5 }").unwrap();
        let (adjustments, _) =
            recipe_for(&Develop::LightroomXmp(plain), &[], path).expect("translated");
        assert!(
            adjustments
                .get(rawtonic_interop::lightroom_hdr::LIGHTROOM_HDR_KEY)
                .is_none()
        );
    }

    /// A catalog's Guided Upright (its develop settings as Lua) keeps its
    /// guides on the way to the XMP the import reads them from.
    #[test]
    fn lightroom_catalog_guided_upright_keeps_its_guides() {
        let lua = r#"s = { PerspectiveUpright = 5, UprightFourSegmentsCount = 2, UprightFourSegments_0 = "0.2,0.1,0.25,0.9", UprightFourSegments_1 = "0.8,0.1,0.75,0.9", UprightVersion = 151388160 }"#;
        let xmp = rawtonic_interop::lightroom_settings::to_xmp(lua).unwrap();
        assert_eq!(
            rawtonic_engine::lightroom_upright::segments(&xmp),
            vec![[0.2, 0.1, 0.25, 0.9], [0.8, 0.1, 0.75, 0.9]],
            "{xmp}"
        );
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
                // A copy of a photograph that is not there: no member, no
                // claim that its photograph was added.
                photo("4", photos.join("Gone.NEF"), Develop::None, Some("2")),
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
                    photos: vec!["1".into(), "3".into(), "4".into()],
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
            stacks: vec![],
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
            (1, 1, 1, 1, 1, 2)
        );
        // What was left out or became something else is named, never silent.
        assert_eq!(
            report.smart_albums_skipped,
            ["Recently edited: no counterpart for “touchTime” (inLast)"]
        );
        assert_eq!(report.copies_in_albums, ["Italy: A.NEF"]);
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
            stacks: vec![],
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
            a["toneVersion"].as_u64(),
            Some(rawtonic_engine::image_processing::TONE_VERSION)
        );
        // Every tool's response a new edit gets.
        for (key, version) in [
            (
                rawtonic_engine::image_processing::CLARITY_VERSION_KEY,
                rawtonic_engine::image_processing::CLARITY_VERSION,
            ),
            (
                rawtonic_engine::image_processing::TEMPERATURE_VERSION_KEY,
                rawtonic_engine::image_processing::TEMPERATURE_VERSION,
            ),
            (
                rawtonic_engine::image_processing::TINT_VERSION_KEY,
                rawtonic_engine::image_processing::TINT_VERSION,
            ),
            (
                rawtonic_engine::image_processing::VIBRANCE_VERSION_KEY,
                rawtonic_engine::image_processing::VIBRANCE_VERSION,
            ),
            (
                rawtonic_engine::image_processing::STRUCTURE_VERSION_KEY,
                rawtonic_engine::image_processing::STRUCTURE_VERSION,
            ),
            (
                rawtonic_engine::image_processing::DEHAZE_VERSION_KEY,
                rawtonic_engine::image_processing::DEHAZE_VERSION,
            ),
            (
                rawtonic_engine::color_noise::VERSION_KEY,
                rawtonic_engine::color_noise::VERSION,
            ),
            (
                rawtonic_engine::image_processing::LUMA_NOISE_VERSION_KEY,
                rawtonic_engine::image_processing::LUMA_NOISE_VERSION,
            ),
            (
                rawtonic_engine::image_processing::DEFRINGE_VERSION_KEY,
                rawtonic_engine::image_processing::DEFRINGE_VERSION,
            ),
            (
                rawtonic_engine::image_processing::GRAIN_VERSION_KEY,
                rawtonic_engine::image_processing::GRAIN_VERSION,
            ),
            (
                rawtonic_engine::image_processing::BW_VERSION_KEY,
                rawtonic_engine::image_processing::BW_VERSION,
            ),
            (
                rawtonic_engine::image_processing::HSL_VERSION_KEY,
                rawtonic_engine::image_processing::HSL_VERSION,
            ),
            (
                rawtonic_engine::image_processing::COLOR_GRADING_VERSION_KEY,
                rawtonic_engine::image_processing::COLOR_GRADING_VERSION,
            ),
        ] {
            assert_eq!(a[key].as_u64(), Some(version), "{key}");
        }
        // Not a RAW: no AI Denoise version.
        assert!(a.get("aiDenoiseVersion").is_none());
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
                stacks: vec![],
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

    #[test]
    fn lightroom_stacks_come_with_top_order_copies_and_once() {
        let tmp = tempfile::tempdir().unwrap();
        let catalog = Library::create(&tmp.path().join("Rawtonic"), "Main", Mode::Catalog).unwrap();
        let photos = tmp.path().join("Photos");
        fs::create_dir_all(&photos).unwrap();
        for name in ["A.NEF", "B.NEF", "C.NEF", "D.NEF", "E.NEF"] {
            fs::write(photos.join(name), name).unwrap();
        }
        let p = |key: &str, name: &str, copy_of: Option<&str>| {
            let mut photo = photo(key, photos.join(name), Develop::None, copy_of);
            photo.uid = Some(format!("UID-{key}"));
            photo
        };
        let stack = |key: &str, collapsed: bool, photos: &[&str]| rawtonic_interop::Stack {
            key: key.into(),
            collapsed,
            photos: photos.iter().map(|k| k.to_string()).collect(),
        };
        let source = Source {
            name: "LR".into(),
            photos: vec![
                p("1", "A.NEF", None),
                p("2", "B.NEF", None),
                p("3", "C.NEF", None),
                p("4", "A.NEF", Some("1")),
                p("5", "D.NEF", None),
                p("6", "E.NEF", None),
                p("7", "Gone.NEF", None),
            ],
            albums: vec![],
            stacks: vec![
                // B on top, A, A's virtual copy, a photo not found.
                stack(
                    "507100D4-EDD0-42A1-9480-82D729286BA8",
                    true,
                    &["2", "1", "4", "7"],
                ),
                stack(
                    "CAA5AF03-9393-4C52-8773-D9BC668743D8",
                    false,
                    &["3", "5", "6"],
                ),
            ],
            not_read: NotRead::default(),
        };
        let id = |name: &str| catalog.asset_at(&photos.join(name)).unwrap().unwrap().id;
        // The photographer stacked D and E here before the import.
        catalog
            .add_asset(&photos.join("D.NEF"))
            .and_then(|_| catalog.add_asset(&photos.join("E.NEF")))
            .unwrap();
        catalog
            .group_stack(&[Member::photo(id("E.NEF")), Member::photo(id("D.NEF"))])
            .unwrap();
        let report = import_into(&catalog, &source).unwrap();
        assert!(report.errors.is_empty(), "{report:?}");
        assert_eq!(report.stacks, 1, "{report:?}");
        let copy = catalog.copies(&id("A.NEF")).unwrap().pop().unwrap();
        let first = catalog
            .stack_of(&Member::photo(id("B.NEF")))
            .unwrap()
            .unwrap();
        assert!(first.collapsed);
        assert_eq!(
            first.members,
            [
                Member::photo(id("B.NEF")),
                Member::photo(id("A.NEF")),
                Member {
                    asset_id: id("A.NEF"),
                    copy: copy.clone()
                },
            ]
        );
        // C's stack would take D and E out of the photographer's stack:
        // they stay, and C alone is no stack.
        assert!(
            catalog
                .stack_of(&Member::photo(id("C.NEF")))
                .unwrap()
                .is_none()
        );
        let theirs = catalog
            .stack_of(&Member::photo(id("D.NEF")))
            .unwrap()
            .unwrap();
        assert_eq!(theirs.members[0], Member::photo(id("E.NEF")));
        assert_eq!(catalog.stacks().unwrap().len(), 2);
        // The photographer moves A to the top and expands the stack; a
        // repeated import changes nothing and makes no second stack.
        catalog
            .move_to_stack_top(&Member::photo(id("A.NEF")))
            .unwrap();
        catalog
            .set_stacks_collapsed(&[Member::photo(id("A.NEF"))], Some(false))
            .unwrap();
        let before = catalog.stacks().unwrap();
        let again = import_into(&catalog, &source).unwrap();
        assert_eq!(again.stacks, 0, "{again:?}");
        assert_eq!(catalog.stacks().unwrap(), before);
        // The photo not found turns up (relinked): a repeated import adds it
        // to its stack, after the members there.
        fs::write(photos.join("Gone.NEF"), b"g").unwrap();
        let found = import_into(&catalog, &source).unwrap();
        assert_eq!(found.stacks, 1, "{found:?}");
        let completed = catalog
            .stack_of(&Member::photo(id("Gone.NEF")))
            .unwrap()
            .unwrap();
        assert_eq!(completed.members.len(), 4);
        assert_eq!(completed.members[0], Member::photo(id("A.NEF")));
        assert!(!completed.collapsed);
        // Stacks are library data: no photograph was written.
        assert_eq!(fs::read(photos.join("A.NEF")).unwrap(), b"A.NEF");
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
    /// The criterion that keeps a collection out is named, also inside
    /// nested groups, as Lightroom names it.
    #[test]
    fn the_untranslatable_criterion_of_a_smart_collection_is_named() {
        let rules = |combine: &str, rules: Vec<SmartRule>| SmartRules {
            combine: combine.into(),
            rules,
        };
        let nested = rules(
            "intersect",
            vec![
                condition("rating", ">=", SmartValue::Number(3.0)),
                SmartRule::Group(rules(
                    "union",
                    vec![
                        condition("labelColor", "==", SmartValue::Number(1.0)),
                        condition("aspectRatio", "==", SmartValue::Text("landscape".into())),
                    ],
                )),
            ],
        );
        assert_eq!(
            smart_rule_gap(&nested).as_deref(),
            Some("“aspectRatio” (==)")
        );
        assert!(smart_rule(&nested).is_none());
        assert_eq!(
            smart_rule_gap(&rules("sideways", vec![])).as_deref(),
            Some("combination “sideways”")
        );
        let fine = rules(
            "intersect",
            vec![condition("rating", ">=", SmartValue::Number(3.0))],
        );
        assert_eq!(smart_rule_gap(&fine), None);
    }

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

    /// Camera, lens, file name and ISO criteria of a Lightroom smart
    /// collection translate to the catalog's own fields; criteria the
    /// catalog has no field for stay out (`smart_rule_gap` names them).
    #[test]
    fn lightroom_camera_lens_file_name_and_iso_criteria_translate() {
        let one = |criteria: &str, operation: &str, value: SmartValue| {
            smart_rule(&SmartRules {
                combine: "intersect".into(),
                rules: vec![condition(criteria, operation, value)],
            })
            .map(|rule| serde_json::to_value(&rule).unwrap()["rules"][0].clone())
        };
        let text = |t: &str| SmartValue::Text(t.into());
        for (criteria, operation, value, expected) in [
            (
                "camera",
                "words",
                text("Canon EOS"),
                json!({"combine": "all", "rules": [
                {"combine": "any", "rules": [
                    {"field": "camera", "op": "contains", "value": "Canon"},
                    {"field": "make", "op": "contains", "value": "Canon"}]},
                {"combine": "any", "rules": [
                    {"field": "camera", "op": "contains", "value": "EOS"},
                    {"field": "make", "op": "contains", "value": "EOS"}]}]}),
            ),
            (
                "camera",
                "any",
                text("Canon Nikon"),
                json!({"combine": "any", "rules": [
                {"field": "camera", "op": "containsAny", "value": "Canon Nikon"},
                {"field": "make", "op": "containsAny", "value": "Canon Nikon"}]}),
            ),
            (
                "camera",
                "noneOf",
                text("Canon"),
                json!({"combine": "none", "rules": [
                {"field": "camera", "op": "containsAny", "value": "Canon"},
                {"field": "make", "op": "containsAny", "value": "Canon"}]}),
            ),
            (
                "camera",
                "notEmpty",
                SmartValue::None,
                json!({"field": "camera", "op": "notEmpty"}),
            ),
            (
                "lens",
                "words",
                text("TAMRON"),
                json!({"field": "lens", "op": "contains", "value": "TAMRON"}),
            ),
            (
                "lens",
                "beginsWith",
                text("EF"),
                json!({"field": "lens", "op": "startsWith", "value": "EF"}),
            ),
            (
                "lens",
                "notEmpty",
                SmartValue::None,
                json!({"field": "lens", "op": "notEmpty"}),
            ),
            (
                "filename",
                "beginsWith",
                text("17"),
                json!({"field": "filename", "op": "startsWith", "value": "17"}),
            ),
            (
                "filename",
                "endsWith",
                text(".DNG"),
                json!({"field": "filename", "op": "endsWith", "value": ".DNG"}),
            ),
            (
                "isoSpeedRating",
                ">=",
                SmartValue::Number(800.0),
                json!({"field": "iso", "op": "ge", "value": 800.0}),
            ),
            (
                "isoSpeedRating",
                "==",
                SmartValue::Number(100.0),
                json!({"field": "iso", "op": "is", "value": 100.0}),
            ),
        ] {
            assert_eq!(
                one(criteria, operation, value),
                Some(expected),
                "{criteria} {operation}"
            );
        }
        let range = smart_rule(&SmartRules {
            combine: "intersect".into(),
            rules: vec![SmartRule::Condition {
                criteria: "isoSpeedRating".into(),
                operation: "in".into(),
                value: SmartValue::Number(800.0),
                value2: SmartValue::Number(1600.0),
                units: None,
            }],
        })
        .unwrap();
        assert_eq!(
            serde_json::to_value(&range).unwrap()["rules"][0],
            json!({"field": "iso", "op": "between", "value": [800.0, 1600.0]})
        );
        // Not Lightroom's id (it ignores it) and values the field cannot hold.
        assert!(one("iso", ">=", SmartValue::Number(800.0)).is_none());
        assert!(one("isoSpeedRating", ">=", text("800")).is_none());
        assert!(one("camera", "words", SmartValue::Number(1.0)).is_none());
        assert!(one("camera", "sideways", text("x")).is_none());
        // A whole name ("Canon EOS 600D") is not the model the catalog reads.
        assert!(one("camera", "==", text("Canon EOS 600D")).is_none());
    }

    /// The translated rules select photographs by the camera, lens, name
    /// and ISO the catalog read from the files.
    #[test]
    fn translated_camera_lens_file_name_and_iso_rules_select_the_photographs() {
        use rawtonic_catalog::CaptureFacts;
        let tmp = tempfile::tempdir().unwrap();
        let catalog = Library::create(&tmp.path().join("Rawtonic"), "Main", Mode::Catalog).unwrap();
        let photos = tmp.path().join("Photos");
        fs::create_dir_all(&photos).unwrap();
        // The camera model without its maker, as the catalog reads it.
        let rows = [
            ("17a.CR2", "EOS 5D Mark II", "EF50mm f/1.8", 100),
            (
                "18b.CR2",
                "EOS 6D",
                "TAMRON SP 17-50mm F/2.8 DiII A16E",
                1600,
            ),
            ("19c.DNG", "M10", "Summicron-M 1:2/35 ASPH.", 800),
            ("20d.NEF", "D80", "", 400),
        ];
        for (name, ..) in rows {
            fs::write(photos.join(name), name).unwrap();
        }
        let source = Source {
            name: "LR".into(),
            photos: rows
                .iter()
                .map(|(name, ..)| photo(name, photos.join(name), Develop::None, None))
                .collect(),
            albums: vec![],
            stacks: vec![],
            not_read: NotRead::default(),
        };
        let album =
            |key: &str, criteria: &str, operation: &str, word: &str| rawtonic_interop::Album {
                key: key.into(),
                name: format!("{criteria} {word}"),
                parent: None,
                is_group: false,
                photos: vec![],
                smart: Some(SmartRules {
                    combine: "intersect".into(),
                    rules: vec![condition(
                        criteria,
                        operation,
                        SmartValue::Text(word.into()),
                    )],
                }),
            };
        let source = Source {
            albums: vec![
                album("a", "lens", "words", "TAMRON"),
                album("b", "filename", "beginsWith", "1"),
            ],
            ..source
        };
        let report = import_into(&catalog, &source).unwrap();
        // Camera and lens albums are named in the report (filled later,
        // matched by text), the file-name album is not.
        assert!(report.warnings.is_empty(), "{report:?}");
        assert_eq!(report.smart_albums_by_facts.len(), 1, "{report:?}");
        assert!(
            report.smart_albums_by_facts[0].starts_with("lens TAMRON: filled as"),
            "{report:?}"
        );
        for (name, model, lens, iso) in rows {
            let id = catalog.asset_at(&photos.join(name)).unwrap().unwrap().id;
            let make = if model.starts_with("EOS") {
                "Canon"
            } else {
                "Leica"
            };
            catalog
                .record_facts(
                    &id,
                    &CaptureFacts {
                        make: Some(make.into()),
                        model: Some(model.into()),
                        lens: (!lens.is_empty()).then(|| lens.into()),
                        iso: Some(iso),
                        ..CaptureFacts::default()
                    },
                )
                .unwrap();
        }
        let members = |criteria: &str, operation: &str, value: SmartValue| {
            let rule = smart_rule(&SmartRules {
                combine: "intersect".into(),
                rules: vec![condition(criteria, operation, value)],
            })
            .unwrap();
            let album = catalog.create_smart_collection("t", None, &rule).unwrap();
            let mut names: Vec<String> = catalog
                .collection_paths(&album.id)
                .unwrap()
                .iter()
                .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        };
        let text = |t: &str| SmartValue::Text(t.into());
        assert_eq!(
            members("camera", "words", text("Canon")),
            ["17a.CR2", "18b.CR2"]
        );
        assert_eq!(
            members("camera", "words", text("canon eos 6d")),
            ["18b.CR2"]
        );
        assert_eq!(
            members("camera", "noneOf", text("Canon")),
            ["19c.DNG", "20d.NEF"]
        );
        assert_eq!(members("lens", "words", text("TAMRON")), ["18b.CR2"]);
        assert_eq!(
            members("filename", "beginsWith", text("1")),
            ["17a.CR2", "18b.CR2", "19c.DNG"]
        );
        assert_eq!(
            members("isoSpeedRating", ">=", SmartValue::Number(800.0)),
            ["18b.CR2", "19c.DNG"]
        );
    }

    /// Capture One's raster masks (unrotated sensor frame) record the
    /// photo file's EXIF orientation on import.
    #[test]
    fn capture_one_rasters_are_imported_with_the_photos_orientation() {
        use rawtonic_interop::capture_one_masks::{Mask, Raster};
        let tmp = tempfile::tempdir().unwrap();
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
        let mut values = vec![0u8; 16 * 8];
        values[3] = 200;
        let layer = rawtonic_interop::capture_one::Layer {
            name: "Brush".into(),
            enabled: true,
            opacity: 100.0,
            settings: vec![("Exposure".into(), "1".into())],
            mask: Mask::Raster {
                raster: Raster::from_values(16, 8, &values),
                ai: false,
            },
            frame: (16.0, 8.0),
        };
        let (recipe, _) = recipe_for(&Develop::None, &[layer], &path).unwrap();
        let sub = &recipe["masks"][0]["subMasks"][0];
        assert_eq!(sub["type"], "raster");
        assert_eq!(sub["parameters"]["sourceOrientation"], 8);
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
            stacks: vec![],
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
    fn lightroom_catalog_point_color_reaches_the_photo_and_its_mask() {
        // Lightroom Classic 15.6 keeps the photo's swatches as tables and a
        // mask's as strings in its catalog (values written for this test).
        let swatch = "0.532900, 0.510200, 0.375800, 0.600000, 0.000000, 0.000000, 0.500000, 0.000000, 0.333333, 0.666667, 1.000000, 0.000000, 0.330200, 1.000000, 1.000000, 0.000000, 0.465912, 0.825912, 1.000000";
        let lua = format!(
            r#"s = {{ MaskGroupBasedCorrections = {{ {{ CorrectionActive = true, CorrectionAmount = 1,
CorrectionMasks = {{ {{ FullX = 0.4, FullY = 0.5, MaskValue = 1, What = "Mask/Gradient", ZeroX = 0.6, ZeroY = 0.5 }} }},
LocalColorVariance = {{ "-0.500000" }},
LocalPointColors = {{ "{swatch}" }},
What = "Correction" }} }},
PointColors = {{ {{ HueRange = {{ LowerFull = 0.33, LowerNone = 0, UpperFull = 0.67, UpperNone = 1 }},
HueShift = 0.3, LumRange = {{ LowerFull = 0.47, LowerNone = 0, UpperFull = 0.83, UpperNone = 1 }},
LumScale = 0, RangeAmount = 0.5,
SatRange = {{ LowerFull = 0.33, LowerNone = 0, UpperFull = 1, UpperNone = 1 }},
SatScale = -0.5, SrcHue = 0.5329, SrcLum = 0.3758, SrcSat = 0.5102, Variance = -0.3 }} }},
ProcessVersion = "15.4" }}"#
        );
        let xmp = rawtonic_interop::lightroom_settings::to_xmp(&lua).unwrap();
        let adjustments = translate(&Develop::LightroomXmp(xmp)).unwrap();
        let photo = &adjustments["colorRanges"][0];
        assert!(photo["hueShift"].as_f64().unwrap() > 0.0, "{photo}");
        assert!(photo["saturation"].as_f64().unwrap() < 0.0, "{photo}");
        assert_eq!(photo["hueUniformity"], 30.0);
        let layer = adjustments["masks"][0]["adjustments"]["colorRanges"]
            .as_array()
            .unwrap();
        assert_eq!(layer.len(), 1);
        assert!(layer[0]["hueShift"].as_f64().unwrap() > photo["hueShift"].as_f64().unwrap());
        assert_eq!(layer[0]["hueUniformity"], 50.0);
    }

    #[test]
    fn lightroom_catalog_keeps_eight_point_color_swatches_and_reports_more() {
        // Lightroom takes up to eight swatches per photo and per mask (its
        // catalog form: tables for the photo, strings for a mask); all eight
        // arrive in order, a ninth (only from a record edited elsewhere) is
        // named in the import report.
        let hue = |i: usize| 0.1 + 0.72 * i as f64;
        let table = |i: usize| {
            format!(
                "{{ HueRange = {{ LowerFull = 0.33, LowerNone = 0, UpperFull = 0.67, UpperNone = 1 }}, HueShift = 0.3, LumRange = {{ LowerFull = 0.4, LowerNone = 0, UpperFull = 0.76, UpperNone = 1 }}, LumScale = 0, RangeAmount = 0.5, SatRange = {{ LowerFull = 0.52, LowerNone = 0, UpperFull = 1, UpperNone = 1 }}, SatScale = -0.5, SrcHue = {:.4}, SrcLum = 0.3, SrcSat = 0.7, Variance = 0 }}",
                hue(i)
            )
        };
        let string = |i: usize| {
            format!(
                "\"{:.6}, 0.700000, 0.300000, -0.300000, 0.000000, 0.000000, 0.500000, 0.000000, 0.333333, 0.666667, 1.000000, 0.000000, 0.520000, 1.000000, 1.000000, 0.000000, 0.400000, 0.760000, 1.000000\"",
                hue(i)
            )
        };
        let lua = |layer: usize| {
            format!(
                r#"s = {{ MaskGroupBasedCorrections = {{ {{ CorrectionActive = true, CorrectionAmount = 1,
CorrectionMasks = {{ {{ FullX = 0.4, FullY = 0.5, MaskValue = 1, What = "Mask/Gradient", ZeroX = 0.6, ZeroY = 0.5 }} }},
LocalPointColors = {{ {} }},
What = "Correction" }} }},
PointColors = {{ {} }},
ProcessVersion = "15.4" }}"#,
                (0..layer)
                    .map(|i| string(i % 8))
                    .collect::<Vec<_>>()
                    .join(", "),
                (0..8).map(table).collect::<Vec<_>>().join(", "),
            )
        };
        let xmp = rawtonic_interop::lightroom_settings::to_xmp(&lua(8)).unwrap();
        let develop = Develop::LightroomXmp(xmp);
        let adjustments = translate(&develop).unwrap();
        let hues = |ranges: &serde_json::Value| -> Vec<f64> {
            ranges
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r["hue"].as_f64().unwrap())
                .collect()
        };
        let photo_hues = hues(&adjustments["colorRanges"]);
        let layer = hues(&adjustments["masks"][0]["adjustments"]["colorRanges"]);
        assert_eq!(photo_hues.len(), 8, "{photo_hues:?}");
        assert_eq!(layer.len(), 8, "{layer:?}");
        // The same source colours in the same order (the shifts differ).
        for (p, l) in photo_hues.iter().zip(&layer) {
            assert!((p - l).abs() < 1e-6, "{photo_hues:?} {layer:?}");
        }
        let path = PathBuf::from("/photos/A.dng");
        assert!(import_warnings(&photo("1", path.clone(), develop, None)).is_empty());
        let nine =
            Develop::LightroomXmp(rawtonic_interop::lightroom_settings::to_xmp(&lua(9)).unwrap());
        let layer = translate(&nine).unwrap()["masks"][0]["adjustments"]["colorRanges"]
            .as_array()
            .unwrap()
            .len();
        assert_eq!(layer, 8);
        let warnings = import_warnings(&photo("1", path, nine, None));
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0].contains("A.dng: Point Color: could not convert 1 swatch(es)"),
            "{warnings:?}"
        );
    }

    /// A Lightroom mask the import leaves out (here a Landscape mask with
    /// the Snow part, which has no equivalent) is named in the report.
    #[test]
    fn skipped_lightroom_masks_are_named_in_the_report() {
        let tmp = tempfile::tempdir().unwrap();
        let catalog = Library::create(&tmp.path().join("Rawtonic"), "Main", Mode::Catalog).unwrap();
        let path = tmp.path().join("IMG_3.jpg");
        let mut jpeg = Vec::new();
        image::DynamicImage::new_rgb8(16, 8)
            .write_to(
                &mut std::io::Cursor::new(&mut jpeg),
                image::ImageFormat::Jpeg,
            )
            .unwrap();
        fs::write(&path, &jpeg).unwrap();
        let landscape = |id: u32| {
            format!(
                r#"<rdf:li crs:What="Mask/Image" crs:MaskSubType="0" crs:MaskSubCategoryID="{id}"/>"#
            )
        };
        let xmp = format!(
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
 <rdf:Description xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" crs:ProcessVersion="15.4">
 <crs:MaskGroupBasedCorrections><rdf:Seq>
  <rdf:li><rdf:Description crs:CorrectionName="Peaks" crs:LocalExposure2012="-0.1"><crs:CorrectionMasks><rdf:Seq>{}{}</rdf:Seq></crs:CorrectionMasks></rdf:Description></rdf:li>
  <rdf:li><rdf:Description crs:CorrectionName="Lake" crs:LocalExposure2012="0.1"><crs:CorrectionMasks><rdf:Seq>{}</rdf:Seq></crs:CorrectionMasks></rdf:Description></rdf:li>
 </rdf:Seq></crs:MaskGroupBasedCorrections></rdf:Description></rdf:RDF></x:xmpmeta>"#,
            landscape(50002),
            landscape(50008),
            landscape(50007),
        );
        let source = Source {
            name: "LR".into(),
            photos: vec![photo("1", path.clone(), Develop::LightroomXmp(xmp), None)],
            albums: vec![],
            stacks: vec![],
            not_read: NotRead::default(),
        };
        let report = import_into(&catalog, &source).unwrap();
        assert_eq!((report.imported, report.ai_masks_on_open), (1, 1));
        assert!(
            report
                .warnings
                .iter()
                .any(|w| w.contains("Lightroom masks not imported") && w.contains("Peaks")),
            "{:?}",
            report.warnings
        );
        let asset = catalog.asset_at(&path).unwrap().unwrap();
        let state = catalog.editor_state(&asset.id).unwrap().unwrap();
        let masks = state["adjustments"]["masks"].as_array().unwrap();
        assert_eq!(masks.len(), 1);
        assert_eq!(
            masks[0]["subMasks"][0]["parameters"]["parts"],
            json!(["water"])
        );
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
            stacks: vec![],
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
            stacks: vec![],
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

    /// Writes the edit the import makes for every photograph and variant of
    /// the Capture One session or catalog `$C1_IMPORT_DB` to
    /// `$C1_IMPORT_OUT/<variant>.json` (as imported, geometry pending) and
    /// the import report's lines to `$C1_IMPORT_OUT/warnings.txt`.
    #[test]
    #[ignore = "tool: needs C1_IMPORT_DB and C1_IMPORT_OUT"]
    fn import_capture_one_edits() {
        let var = |k: &str| std::env::var(k).unwrap();
        // The lens database the app loads at start (Capture One's lens
        // correction on lensfun profiles).
        if let Ok(db) = std::env::var("RAWTONIC_LENSFUN_DB") {
            rawtonic_engine::lens_correction::set_global_lens_db(std::sync::Arc::new(
                rawtonic_engine::lens_correction::load_lensfun_dir(Path::new(&db)),
            ));
        }
        let (source, _) = read_source(Path::new(&var("C1_IMPORT_DB"))).unwrap();
        let out = PathBuf::from(var("C1_IMPORT_OUT"));
        let mut warnings = Vec::new();
        for photo in &source.photos {
            let (recipe, _) = recipe_for(&photo.develop, &photo.capture_one_layers, &photo.path)
                .unwrap_or((json!({}), false));
            rawtonic_export::output::write_new_output(
                out.join(format!("{}.json", photo.key)),
                recipe.to_string(),
            )
            .unwrap();
            warnings.extend(import_warnings(photo));
        }
        rawtonic_export::output::write_new_output(out.join("warnings.txt"), warnings.join("\n"))
            .unwrap();
        println!(
            "{} photos, not read {:?}",
            source.photos.len(),
            source.not_read
        );
    }

    /// A catalog folder named with dots (`Wedding.2026.cocatalog`) is read
    /// from the database inside it (`Wedding.2026.cocatalogdb`).
    #[test]
    fn capture_one_catalogs_with_dotted_names_are_read() {
        let dir = tempfile::tempdir().unwrap();
        let catalog = dir.path().join("Wedding.2026.cocatalog");
        let db = capture_one_session(dir.path(), "Wedding.2026");
        fs::create_dir_all(&catalog).unwrap();
        fs::copy(&db, catalog.join("Wedding.2026.cocatalogdb")).unwrap();
        let (source, kind) = read_source(&catalog).unwrap();
        assert_eq!(kind, "captureOne");
        assert_eq!(source.name, "Wedding.2026");
    }

    #[test]
    fn sessions_are_refused_as_targets() {
        let tmp = tempfile::tempdir().unwrap();
        let session = Library::create(&tmp.path().join("Job"), "Job", Mode::Session).unwrap();
        let source = Source {
            name: "x".into(),
            photos: vec![],
            albums: vec![],
            stacks: vec![],
            not_read: NotRead::default(),
        };
        assert!(import_into(&session, &source).is_err());
    }

    /// Smart collection membership of an imported Lightroom catalog, as the
    /// app shows it (`SMR_LRCAT`, photographs at their catalog paths,
    /// `SMR_OUT` the scratch catalog and result files, which must not exist
    /// yet). Importing writes the copies' `.rrdata` beside their
    /// photographs: run it on scratch copies of the photographs.
    #[test]
    #[ignore = "tool: needs SMR_LRCAT and SMR_OUT"]
    fn smart_collection_membership_of_a_lightroom_catalog() {
        use std::sync::atomic::AtomicBool;
        let lrcat = std::env::var("SMR_LRCAT").unwrap();
        let out = std::path::PathBuf::from(std::env::var("SMR_OUT").unwrap());
        fs::create_dir_all(&out).unwrap();
        let catalog = Library::create(&out.join("Scratch"), "Scratch", Mode::Catalog).unwrap();
        let (lib, kind) = read_source(Path::new(&lrcat)).unwrap();
        let shown = preview(&lib, kind);
        let report = import_into(&catalog, &lib).unwrap();
        // The app's background pass: capture facts (dates, camera, lens).
        let cancel = AtomicBool::new(false);
        let mut cursor = 0;
        loop {
            let (looked, _, _) = catalog
                .fill_facts(
                    &mut cursor,
                    256,
                    std::time::Duration::ZERO,
                    &cancel,
                    &rawtonic_ingest::capture::capture_facts,
                    &|| (),
                )
                .unwrap();
            if looked == 0 {
                break;
            }
        }
        let mut tsv = String::new();
        for c in catalog.collections().unwrap() {
            let paths: Vec<String> = catalog
                .collection_assets(&c.id)
                .map(|a| {
                    a.into_iter()
                        .map(|a| a.path.to_string_lossy().into_owned())
                        .collect()
                })
                .unwrap_or_else(|e| vec![format!("ERROR {e}")]);
            tsv += &format!(
                "{}\t{}\t{}\t{}\t{}\n",
                c.id,
                c.name,
                c.kind,
                paths.len(),
                paths.join(",")
            );
        }
        let write_new = |name: &str, text: String| {
            use std::io::Write;
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(out.join(name))
                .unwrap()
                .write_all(text.as_bytes())
                .unwrap();
        };
        write_new("rawtonic-collections.tsv", tsv);
        write_new(
            "preview-and-report.txt",
            format!("{shown:#?}\n{report:#?}\n"),
        );
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
        fs::write(
            out.join("stacks.json"),
            serde_json::to_vec_pretty(&catalog.listed_stacks().unwrap()).unwrap(),
        )
        .unwrap();
        // A repeated import makes no second stack.
        let again = import_into(&catalog, &lib).unwrap();
        println!(
            "again: {} stacks made, {} in the catalog",
            again.stacks,
            catalog.stacks().unwrap().len()
        );
    }
}
