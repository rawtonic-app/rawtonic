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
//!
//! A repeated import never replaces an edit the photographer made here
//! since the last one (C1RI1): each import records digests of what it wrote
//! (`rawtonic_catalog::imported`); an edit still exactly as that import left
//! it follows the other editor's changed settings (one Undo step in the
//! app), one changed here is kept, and when the other editor's settings
//! changed too they come in as a new virtual copy beside it (`reimport`).
use crate::file_management::RecipeChange;
use rawtonic_catalog::stacks::{Member, Stack};
use rawtonic_catalog::{
    ALBUM, Combine, Field, GROUP, ImportRecord, Library, Mode, Op, Rule, SMART,
};
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
    /// Keywords of the source's keyword library (without children), unused
    /// ones too.
    pub keywords: usize,
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
    /// Keywords of the source's keyword library newly kept in the Keyword
    /// List, with or without photographs.
    pub keywords: usize,
    /// Lightroom stacks made (or completed by a repeated import).
    pub stacks: usize,
    /// Photographs with Lightroom AI masks (Select Subject, Select Sky):
    /// their model runs when the photo is opened in Develop, so exports
    /// before that leave those masks' layers without effect.
    pub ai_masks_on_open: usize,
    /// Smart collections left out, each with the criterion that has no
    /// counterpart (never imported with other rules or other members).
    pub smart_albums_skipped: Vec<String>,
    /// Smart albums imported on camera, lens, ISO or capture date: the
    /// catalog fills them as the background pass reads the files, and
    /// camera words are matched in the camera's one make and model, not in
    /// the model alone as Lightroom does, so they can differ from
    /// Lightroom's. (Lens rules use
    /// Lightroom's own lens name for each photograph, kept by the import.)
    pub smart_albums_by_facts: Vec<String>,
    /// Edits here (photographs' and virtual copies') a repeated import
    /// brought up to date because they were still exactly as the last
    /// import left them: each one's whole recipe before and after, which the
    /// app's Undo writes back as one step.
    pub recipe_changes: Vec<crate::file_management::RecipeChange>,
    /// Edits changed here since the last import (or never imported) whose
    /// settings in the other editor changed too: kept as they are, the
    /// other editor's settings imported as a new virtual copy beside them,
    /// one line each.
    pub edits_kept: Vec<String>,
    /// Photographs (and virtual copies) kept as imported earlier whose
    /// straightening Capture One shows and an earlier import did not read
    /// (`Settled::unread_straightening`): their paths as in the warning's
    /// line, for "Apply Capture One straightening and crop"
    /// (`library_apply_source_geometry`).
    pub geometry_offers: Vec<GeometryOffer>,
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
        keywords: source.keywords.len(),
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
/// capture date. Lightroom may name a camera differently from what the file
/// states, so those members can differ (lens rules match Lightroom's own
/// lens names, kept by the import).
fn smart_rule_facts_note(rules: &SmartRules) -> Option<&'static str> {
    const TEXT: &str = "filled as the files' capture data is read; Lightroom may name a camera differently from the file, so the members can differ";
    const FACTS: &str = "filled as the files' capture data is read";
    rules
        .rules
        .iter()
        .filter_map(|rule| match rule {
            SmartRule::Group(group) => smart_rule_facts_note(group),
            SmartRule::Condition { criteria, .. } => match criteria.as_str() {
                "camera" => Some(TEXT),
                "isoSpeedRating" | "aperture" | "focalLength" | "shutterSpeed" | "captureTime" => {
                    Some(FACTS)
                }
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

/// Lightroom compares aperture and focal length within ±0.01 in aperture
/// values (`Av = 2·log2(f-number)`) and millimetres (measured, SMR2).
/// Translate those limits into the catalog's f-number and millimetre bounds.
fn aperture_or_focal_length_rule(
    criteria: &str,
    operation: &str,
    value: f64,
    value2: Option<f64>,
) -> Option<Rule> {
    let (field, tolerance, at): (Field, f64, fn(f64) -> f64) = match criteria {
        "aperture" => (Field::Aperture, 0.01, |av| (av / 2.0).exp2()),
        "focalLength" => (Field::FocalLength, 0.01, |mm| mm),
        _ => return None,
    };
    // The catalog's smallest and largest value Lightroom takes as `x`.
    let low = |x: f64| at(x - tolerance).min(at(x + tolerance));
    let high = |x: f64| at(x - tolerance).max(at(x + tolerance));
    let one = |op: Op, v: f64| Rule::Condition {
        field,
        op,
        value: json!(v),
    };
    let within = |from: f64, to: f64| Rule::Condition {
        field,
        op: Op::Between,
        value: json!([from, to]),
    };
    Some(match operation {
        "==" => within(low(value), high(value)),
        // Not "none of equal": a photograph without the value is not
        // different from it either.
        "!=" => Rule::Group {
            combine: Combine::Any,
            rules: vec![one(Op::Lt, low(value)), one(Op::Gt, high(value))],
        },
        ">" => one(Op::Gt, high(value)),
        ">=" => one(Op::Ge, low(value)),
        "<" => one(Op::Lt, low(value)),
        "<=" => one(Op::Le, high(value)),
        // A range from the larger to the smaller value holds nothing in Lightroom.
        "in" => {
            let (from, to) = (low(value), high(value2?));
            if from > to {
                Rule::Group {
                    combine: Combine::Any,
                    rules: vec![],
                }
            } else {
                within(from, to)
            }
        }
        _ => return None,
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
        // Lightroom's camera name is the model as the camera writes it
        // ("Canon EOS 600D", "NIKON D750", "ILCE-7M3"; measured, SMR5); the
        // catalog keeps one make and model per camera
        // (`rawtonic_raw_format::camera_names`), so a word is looked for in
        // either, and a whole name is that camera's one make and model.
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
                // Lightroom's "words": every word is a whole word of the name;
                // "all": every word is part of it (measured, SMR2).
                "words" | "all" => Some(Rule::Group {
                    combine: Combine::All,
                    rules: text(value)?
                        .split_whitespace()
                        .map(|word| {
                            either(
                                if operation == "words" {
                                    Op::Words
                                } else {
                                    Op::Contains
                                },
                                word,
                            )
                        })
                        .collect(),
                }),
                "any" => Some(either(Op::ContainsAny, &text(value)?)),
                "noneOf" => Some(Rule::Group {
                    combine: Combine::None,
                    rules: in_either(Op::ContainsAny, &text(value)?),
                }),
                // Lightroom ignores case; the catalog's "is" does too.
                "==" | "!=" => {
                    use rawtonic_raw_format::camera_names;
                    let name = text(value)?;
                    let rules = match camera_names::canonical_of_model(&name) {
                        // The library's one camera for several models its
                        // files state (Olympus `STYLUS1` and `STYLUS1,1s`):
                        // Lightroom tells them apart, so the rule would
                        // take more photographs than in Lightroom; left
                        // out (named in the import report).
                        Some((make, model))
                            if camera_names::stated_names(&make, &model).iter().any(
                                |(_, file_model)| *file_model != name.trim().to_lowercase(),
                            ) =>
                        {
                            return None;
                        }
                        Some((make, model)) => vec![
                            Rule::Condition {
                                field: Field::Make,
                                op: Op::Is,
                                value: json!(make),
                            },
                            Rule::Condition {
                                field: Field::Camera,
                                op: Op::Is,
                                value: json!(model),
                            },
                        ],
                        // Only the library's name for a camera whose files
                        // state another (`D750` for `NIKON D750`): no
                        // photograph's model in Lightroom (measured, SMR5).
                        None if camera_names::is_canonical_model(&name) => vec![Rule::Group {
                            combine: Combine::Any,
                            rules: vec![],
                        }],
                        // A camera the decoder does not know keeps the
                        // names its files give.
                        None => vec![Rule::Condition {
                            field: Field::Camera,
                            op: Op::Is,
                            value: json!(name.trim()),
                        }],
                    };
                    Some(Rule::Group {
                        combine: if operation == "==" {
                            Combine::All
                        } else {
                            Combine::None
                        },
                        rules: if operation == "==" {
                            rules
                        } else {
                            vec![Rule::Group {
                                combine: Combine::All,
                                rules,
                            }]
                        },
                    })
                }
                // The model's start or end (Lightroom's camera is the EXIF
                // model; the catalog's camera rule reads the camera's one
                // model and the models its files state). Left out (named in
                // the import report) where the two would select different
                // files: a test that splits one camera here (Olympus
                // `STYLUS1` / `STYLUS1,1s`) or holds for its one model only.
                "beginsWith" | "endsWith" => {
                    let affix = text(value)?;
                    let folded = affix.trim().to_lowercase();
                    let begins = operation == "beginsWith";
                    let agrees =
                        rawtonic_raw_format::camera_names::model_test_agrees_per_file(|model| {
                            if begins {
                                model.starts_with(&folded)
                            } else {
                                model.ends_with(&folded)
                            }
                        });
                    if !agrees || folded.is_empty() {
                        return None;
                    }
                    Some(Rule::Condition {
                        field: Field::Camera,
                        op: if begins { Op::StartsWith } else { Op::EndsWith },
                        value: json!(affix),
                    })
                }
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
        // Copy status: virtual copies, or masters (every photograph that is
        // not a copy; measured, SMR5).
        "vcStatus" => {
            let copies = match text(value)?.as_str() {
                "virtualCopies" => true,
                "masterImages" => false,
                _ => return None,
            };
            // "is not" as "is" with the other answer, the form the
            // smart-album editor shows.
            let copies = match compare(operation)? {
                Op::Is => copies,
                Op::IsNot => !copies,
                _ => return None,
            };
            (Field::VirtualCopy, Op::Is, json!(copies))
        }
        // Text of the photograph's own facts: Lightroom's word search and
        // comparisons, as the catalog's.
        "lens" | "filename" => {
            let op = match operation {
                "words" if criteria == "lens" => Op::Words,
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
            // Lightroom matches a lens rule against its own name for the lens
            // (kept per photograph by the import), not against the name the
            // file states.
            let field = if criteria == "lens" {
                Field::LensImported
            } else {
                Field::Filename
            };
            let value = match op {
                Op::Empty | Op::NotEmpty => Value::Null,
                _ => json!(text(value)?),
            };
            (field, op, value)
        }
        "aperture" | "focalLength" => {
            return aperture_or_focal_length_rule(
                criteria,
                operation,
                number(value)?,
                number(value2),
            );
        }
        "shutterSpeed" => {
            // Lightroom stores Tv = log2(1/seconds), but orders by seconds:
            // greater means a longer time, and a range starts at the shorter time.
            let from = number(value)?;
            let seconds = |tv: f64| (-tv).exp2();
            let (op, value) = if operation == "in" {
                let to = number(value2)?;
                if from < to {
                    return Some(Rule::Group {
                        combine: Combine::Any,
                        rules: vec![],
                    });
                }
                (Op::Between, json!([seconds(from), seconds(to)]))
            } else {
                (compare(operation)?, json!(seconds(from)))
            };
            (Field::ExposureExact, op, value)
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
            .chain(capture_one_lens_report(entries, &photo.path))
            .chain(capture_one_rotation_report(entries, &photo.path))
            .chain(capture_one_color_editor_report(entries))
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
                settle_reimport(
                    catalog,
                    &mut report,
                    &asset,
                    &source.name,
                    photo,
                    None,
                    imported,
                );
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
                        copy: copy.clone(),
                    },
                );
                report.copies += 1;
                report.ai_masks_on_open += usize::from(imported.ai_masks);
                report.crops_clipped += imported.crops_clipped;
                report.warnings.extend(import_warnings(photo));
                settle_reimport(
                    catalog,
                    &mut report,
                    asset,
                    &source.name,
                    photo,
                    Some(&copy),
                    imported,
                );
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
                // A virtual copy is a member by itself, as in Lightroom.
                let items: Vec<(String, Option<String>)> = album
                    .photos
                    .iter()
                    .filter_map(|key| {
                        let member = members.get(key.as_str())?;
                        Some((
                            member.asset_id.clone(),
                            (!member.copy.is_empty()).then(|| member.copy.clone()),
                        ))
                    })
                    .collect();
                if !items.is_empty() {
                    catalog
                        .add_items_to_collection(&id, &items)
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
    // The keyword library whole, unused terms too (KWC1): a keyword list is
    // part of the work a photographer brings over.
    report.keywords = catalog
        .apply_declared_keywords(&[], &source.keywords)
        .map_err(|e| e.to_string())?
        .added
        .len();
    Ok(report)
}

/// The repeated-import part of one photograph's or copy's import: its
/// brought-up-to-date edit for the Undo step (its derived sidecar follows),
/// or the new virtual copy with the other editor's newer settings beside a
/// kept edit, with its line.
fn settle_reimport(
    catalog: &Library,
    report: &mut ImportReport,
    asset: &str,
    source_name: &str,
    photo: &Photo,
    copy: Option<&str>,
    imported: Imported,
) {
    report.errors.extend(imported.error);
    report.warnings.extend(imported.warning);
    report.geometry_offers.extend(imported.geometry_offer);
    // A copy's sidecar followed in `reimport_copy` already.
    if let Some(change) = imported.change {
        match copy.map_or_else(|| follow_sidecar(catalog, asset, &change), |_| Ok(true)) {
            Ok(true) => report.recipe_changes.push(change),
            // Changed beside the photo between the check and the write: the
            // library's update stays, but no Undo step writes over that file.
            Ok(false) => report.errors.push(changed_elsewhere(&change)),
            // The library holds a photograph's own edit, as after an edit
            // whose sidecar could not be written (`DerivedSidecarUnwritten`).
            Err(e) => {
                report.errors.push(format!("{}: {e}", photo.path.display()));
                report.recipe_changes.push(change);
            }
        }
    }
    if imported.kept_with_newer {
        match import_newer_copy(catalog, asset, source_name, photo, copy) {
            Ok(Some(line)) => {
                report.copies += 1;
                report.edits_kept.push(line);
            }
            Ok(None) => {}
            Err(e) => report.errors.push(format!("{}: {e}", photo.path.display())),
        }
    }
}

fn changed_elsewhere(change: &RecipeChange) -> String {
    format!(
        "{}: the editor settings file beside the photo was changed elsewhere since; \
         left as it is",
        photo_ref::PhotoRef::parse(&change.path)
            .sidecar_path()
            .display()
    )
}

/// The derived editor sidecar beside the photograph (`<file>.rrdata`, a
/// copy's `<file>.<id>.rrdata`) of an edit the import brought up to date
/// follows the library: published when missing, replaced (atomically) only
/// while it still holds the edit the library had before (`change.before`).
/// One another program or catalog changed since is left as it is (false).
/// An offline photograph's follows when it is back (`session_bridge::rederive`).
/// The photograph itself is never written.
fn follow_sidecar(catalog: &Library, asset: &str, change: &RecipeChange) -> anyhow::Result<bool> {
    let photo = photo_ref::PhotoRef::parse(&change.path);
    if !photo.source_path().is_file() {
        return Ok(true);
    }
    let state = match photo.copy_id() {
        Some(copy) => catalog.copy_state(asset, copy)?,
        None => catalog.editor_state(asset)?,
    };
    let Some(state) = state else {
        return Ok(true);
    };
    let bytes = serde_json::to_vec_pretty(&state)?;
    let sidecar = photo.sidecar_path();
    let written = match std::fs::read(&sidecar) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => publish_new(&sidecar, &bytes),
        Err(e) => return Err(e.into()),
        Ok(current) => {
            let current: Value = serde_json::from_slice(&current).unwrap_or(Value::Null);
            if edit_digest(current.get("adjustments")) != edit_digest(Some(&change.before)) {
                return Ok(false);
            }
            rawtonic_fs::atomic_write(&sidecar, &bytes)
        }
    };
    match written {
        Ok(()) => Ok(true),
        // Failed after the new bytes were in place (the folder's sync): the
        // file holds the update, as an editor save would judge it.
        Err(_) if std::fs::read(&sidecar).is_ok_and(|now| now == bytes) => Ok(true),
        Err(e) => Err(e),
    }
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
    /// The photograph's or copy's edit here was kept, and the other
    /// editor's changed settings for it are to come in as a new virtual
    /// copy (`import_newer_copy`).
    kept_with_newer: bool,
    /// The edit here was brought up to date: its recipe before and after.
    change: Option<RecipeChange>,
    /// Why an update was not made after all, for the summary's errors.
    error: Option<String>,
    /// A line for the summary's warnings (`Settled::unread_straightening`).
    warning: Option<String>,
    /// The path of that line (`ImportReport::geometry_offers`).
    geometry_offer: Option<GeometryOffer>,
}

/// What the import report shows for a photo it offers "Apply Capture One
/// straightening and crop" for: the action applies exactly this, and writes
/// nothing if Capture One shows another geometry by then.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeometryOffer {
    pub path: String,
    /// Straightening angle in degrees.
    pub rotation: f64,
    pub orientation_steps: f64,
}

impl GeometryOffer {
    fn of(path: String, incoming: Option<&Value>) -> Self {
        Self {
            path,
            rotation: geometry_value(incoming, geometry::ROTATION.name()),
            orientation_steps: geometry_value(incoming, geometry::ORIENTATION_STEPS.name()),
        }
    }

    fn as_recipe(&self) -> Value {
        json!({
            geometry::ROTATION.name(): self.rotation,
            geometry::ORIENTATION_STEPS.name(): self.orientation_steps,
        })
    }
}

/// What a repeated import does with an edit already here (decision C1RI1
/// 2026-11-23, a professional's view: least surprise, never lost work).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reimport {
    /// No edit of the photographer's here, or one still exactly as the
    /// last import left it: the import's edit replaces it (one Undo step).
    Apply,
    /// The edit here is the import's already: nothing to write; recorded.
    Same,
    /// Changed here since, the other editor's settings unchanged: kept.
    Keep,
    /// Changed here since (or never imported), and the other editor's
    /// settings changed too: kept, those come in as a new virtual copy.
    KeepWithNewer,
}

/// The rule: `record` is what the last import wrote to the version (none:
/// never imported by a build that records it), `own` its recipe here,
/// `source` the other editor's settings by digest (`source_digest`),
/// `incoming` the recipe made of them (None: nothing to translate, which
/// never clears an edit: the reader cannot tell a reset there from settings
/// it could not read). "Changed here" is told from the recorded digest,
/// never from times; settings unchanged there (by their own digest) write
/// nothing, whatever a newer translation would make of them.
fn reimport(
    record: Option<&ImportRecord>,
    own: Option<&Value>,
    source: &str,
    incoming: Option<&Value>,
) -> Reimport {
    if !own.is_some_and(is_edit) {
        return Reimport::Apply;
    }
    let own = edit_digest(own);
    let new = edit_digest(incoming);
    if own == new {
        return Reimport::Same;
    }
    match record {
        _ if !incoming.is_some_and(is_edit) => Reimport::Keep,
        Some(r) if r.source == source => Reimport::Keep,
        Some(r) if r.stored == own => Reimport::Apply,
        Some(r) if r.translated == new => Reimport::Keep,
        _ => Reimport::KeepWithNewer,
    }
}

/// `reimport` for an edit whose derived sidecar beside the photograph may
/// hold another edit than the library (`sidecar`, from `sidecar_edit`):
/// one another catalog over the same folder (or another program) saved
/// there, which the editor reads for a photograph that is online. It is
/// the photographer's too: the library's edit is never replaced then, and
/// changed settings there come in as a new virtual copy instead.
///
/// A photo whose source the reader could only read in part
/// (`coverage_unreadable`) never replaces an edit here: its translation
/// lacks what could not be read.
fn reimport_here(
    record: Option<&ImportRecord>,
    own: Option<&Value>,
    sidecar: Option<Value>,
    source: &Photo,
    incoming: Option<&Value>,
) -> Reimport {
    let own = match (&sidecar, own) {
        (Some(edit), own) if !own.is_some_and(is_edit) => Some(edit),
        (_, own) => own,
    };
    if own.is_some_and(is_edit) && coverage_unreadable(source) {
        return Reimport::Keep;
    }
    match reimport(record, own, &source_digest(source), incoming) {
        Reimport::Apply if sidecar.is_some() => Reimport::KeepWithNewer,
        rule => rule,
    }
}

/// The edit in the derived sidecar beside the photograph at `path` (a
/// copy's for `copy`) when it is another than the library's `own`.
fn sidecar_edit(path: &Path, copy: Option<&str>, own: Option<&Value>) -> Option<Value> {
    let sidecar = path.with_file_name(photo_ref::sidecar_name(path, copy));
    let state: Value = serde_json::from_slice(&std::fs::read(sidecar).ok()?).ok()?;
    let edit = state.get("adjustments")?;
    (edit_digest(Some(edit)) != edit_digest(own)).then(|| edit.clone())
}

/// Whether a recipe holds an edit (more than the colour label metadata
/// puts into it).
fn is_edit(adjustments: &Value) -> bool {
    adjustments
        .as_object()
        .is_some_and(|map| map.keys().any(|k| k != "colorLabel"))
}

/// SHA-256 of a value's JSON (maps in key order).
fn digest_of(value: &impl Serialize) -> String {
    use sha2::Digest;
    let bytes = serde_json::to_vec(value).unwrap_or_default();
    rawtonic_fs::hex(&sha2::Sha256::digest(bytes)[..])
}

/// The Capture One rasters of the photo's layers (painted, colour range,
/// AI and Luma Range coverage).
fn layer_rasters(
    photo: &Photo,
) -> impl Iterator<Item = &rawtonic_interop::capture_one_masks::Raster> {
    use rawtonic_interop::capture_one_masks::{Coverage, Mask};
    photo
        .capture_one_layers
        .iter()
        .filter_map(|layer| match &layer.mask {
            Mask::Raster { raster, .. }
            | Mask::LumaRange {
                base: Coverage::Raster { raster },
                ..
            } => Some(raster),
            _ => None,
        })
}

/// Whether a layer's coverage could not be read (a damaged or incomplete
/// copy of the session's mask files: a raster tile, a mask record the
/// reader did not understand or did not find): the translation lacks it.
fn coverage_unreadable(photo: &Photo) -> bool {
    use rawtonic_interop::capture_one_masks::Mask;
    photo
        .capture_one_layers
        .iter()
        .any(|layer| matches!(layer.mask, Mask::Unsupported { .. } | Mask::Missing))
        || layer_rasters(photo).any(|raster| raster.values().is_none())
}

/// The other editor's settings for a photo or copy, by digest: its
/// settings, its Capture One layers and their painted coverage (the
/// rasters' values, which their serialized form leaves out).
fn source_digest(photo: &Photo) -> String {
    source_digest_of(photo, &photo.develop, &photo.capture_one_layers)
}

/// Capture One settings the reader takes since G50L (2026-11-24): an
/// import recorded before read the same session without them.
const CAPTURE_ONE_KEYS_SINCE_G50L: [&str; 2] = ["Rotation", "Crop"];

/// The digest an earlier reader recorded for these same settings
/// (`source_digest` without `CAPTURE_ONE_KEYS_SINCE_G50L`), where it differs
/// from today's: settings unchanged in the other editor are no change for
/// a repeated import, whatever the reader now reads of them.
fn earlier_source_digest(photo: &Photo) -> Option<String> {
    let newer = |settings: &[(String, String)]| {
        settings
            .iter()
            .any(|(k, _)| CAPTURE_ONE_KEYS_SINCE_G50L.contains(&k.as_str()))
    };
    let Develop::CaptureOne(entries) = &photo.develop else {
        return None;
    };
    if !newer(entries)
        && !photo
            .capture_one_layers
            .iter()
            .any(|layer| newer(&layer.settings))
    {
        return None;
    }
    let older = |settings: &[(String, String)]| -> Vec<(String, String)> {
        settings
            .iter()
            .filter(|(k, _)| !CAPTURE_ONE_KEYS_SINCE_G50L.contains(&k.as_str()))
            .cloned()
            .collect()
    };
    let layers: Vec<_> = photo
        .capture_one_layers
        .iter()
        .map(|layer| rawtonic_interop::capture_one::Layer {
            settings: older(&layer.settings),
            ..layer.clone()
        })
        .collect();
    Some(source_digest_of(
        photo,
        &Develop::CaptureOne(older(entries)),
        &layers,
    ))
}

/// The import `record` of a version as of today's reader: one an earlier
/// reader made of the same settings (`earlier_source_digest`) holds
/// today's digest, so the settings count as unchanged and a later change
/// in the other editor still shows. True when an earlier reader's digest
/// matched.
fn current_record(record: Option<ImportRecord>, photo: &Photo) -> (Option<ImportRecord>, bool) {
    let Some(mut record) = record else {
        return (None, false);
    };
    let earlier = earlier_source_digest(photo).is_some_and(|earlier| earlier == record.source);
    if earlier {
        record.source = source_digest(photo);
    }
    (Some(record), earlier)
}

/// A repeated import's decision for one version (`reimport_here` on the
/// record as of today's reader).
#[derive(Debug, PartialEq)]
struct Settled {
    rule: Reimport,
    /// The record to store now: an earlier reader's, upgraded.
    upgrade: Option<ImportRecord>,
    /// The version keeps an import an earlier reader made although the
    /// other editor straightens or turns the photo otherwise: that reader
    /// could not see whether it did so then or since, so the edit stays,
    /// the import names it (every time, until the edit matches) and its
    /// record is not upgraded.
    unread_straightening: bool,
}

fn settle_rule(
    record: Option<ImportRecord>,
    own: Option<&Value>,
    sidecar: Option<Value>,
    photo: &Photo,
    incoming: Option<&Value>,
) -> Settled {
    let (current, earlier) = current_record(record.clone(), photo);
    let rule = reimport_here(current.as_ref(), own, sidecar, photo, incoming);
    let unread_straightening = earlier
        && rule == Reimport::Keep
        && straightens(incoming)
        && straightening_differs(own, incoming);
    let upgrade = current.filter(|c| !unread_straightening && record.as_ref() != Some(c));
    Settled {
        rule,
        upgrade,
        unread_straightening,
    }
}

/// Half the Crop tool's straightening step (0.1°): an angle typed there to
/// match an imported one counts as matching it.
const STRAIGHTENING_MATCH: f64 = 0.05;

fn geometry_value(recipe: Option<&Value>, key: &str) -> f64 {
    recipe
        .and_then(|r| r.get(key))
        .and_then(Value::as_f64)
        .unwrap_or(0.0)
}

/// Whether a recipe straightens or turns the photo (an import's: what the
/// other editor does to it).
fn straightens(recipe: Option<&Value>) -> bool {
    geometry_value(recipe, geometry::ROTATION.name()).abs() > STRAIGHTENING_MATCH
        || geometry_value(recipe, geometry::ORIENTATION_STEPS.name()) != 0.0
}

/// Whether two recipes straighten or turn the photo differently (angles
/// within `STRAIGHTENING_MATCH` match).
fn straightening_differs(a: Option<&Value>, b: Option<&Value>) -> bool {
    let rotation = geometry::ROTATION.name();
    let steps = geometry::ORIENTATION_STEPS.name();
    (geometry_value(a, rotation) - geometry_value(b, rotation)).abs() > STRAIGHTENING_MATCH
        || geometry_value(a, steps) != geometry_value(b, steps)
}

/// The import summary's line for `Settled::unread_straightening`.
fn unread_straightening_line(path: &str, incoming: Option<&Value>) -> String {
    let value = |key: &str| {
        incoming
            .and_then(|r| r.get(key))
            .and_then(Value::as_f64)
            .unwrap_or(0.0)
    };
    let turns = value(geometry::ORIENTATION_STEPS.name());
    let turns = if turns != 0.0 {
        format!(", turned {} quarter turn(s) clockwise", turns)
    } else {
        String::new()
    };
    format!(
        "{path}: kept as imported earlier; Capture One shows it straightened by {}°{turns}, which imports before this version did not read: straighten it here to match, or apply Capture One's straightening and crop (a crop of the frame's own aspect ratio without empty corners; a crop drawn in Capture One is not read)",
        value(geometry::ROTATION.name())
    )
}

fn source_digest_of(
    photo: &Photo,
    develop: &Develop,
    layers: &[rawtonic_interop::capture_one::Layer],
) -> String {
    use sha2::Digest;
    let mut digest = sha2::Sha256::new();
    digest.update(serde_json::to_vec(&(develop, layers)).unwrap_or_default());
    for raster in layer_rasters(photo) {
        match raster.values() {
            Some(values) => digest.update(&values),
            None => digest.update(b"unreadable"),
        }
    }
    rawtonic_fs::hex(&digest.finalize()[..])
}

/// A recipe by digest as far as it is the photographer's edit: without the
/// colour label (metadata), the data of AI masks a model computes when the
/// photo is opened (`has_ai_masks_on_open`; painted and other masks' data
/// stays) and the generated ids of masks, layers and patches (each
/// translation makes new ones); no edit is `{}`.
fn edit_digest(adjustments: Option<&Value>) -> String {
    let mut edit = adjustments
        .filter(|a| is_edit(a))
        .cloned()
        .unwrap_or_else(|| json!({}));
    if let Some(map) = edit.as_object_mut() {
        map.remove("colorLabel");
    }
    if let Some(layers) = edit
        .get_mut(masks::MASKS.name())
        .and_then(Value::as_array_mut)
    {
        for sub in layers
            .iter_mut()
            .filter_map(|layer| layer.get_mut(masks::SUB_MASKS.name()))
            .filter_map(Value::as_array_mut)
            .flatten()
        {
            if matches!(
                sub["type"].as_str(),
                Some("ai-foreground" | "ai-sky" | "ai-landscape")
            ) && let Some(parameters) = sub.get_mut("parameters").and_then(Value::as_object_mut)
            {
                parameters.remove("maskDataBase64");
            }
        }
    }
    strip_key(&mut edit, "id");
    digest_of(&edit)
}

fn strip_key(value: &mut Value, key: &str) {
    match value {
        Value::Object(map) => {
            map.remove(key);
            map.values_mut().for_each(|v| strip_key(v, key));
        }
        Value::Array(items) => items.iter_mut().for_each(|v| strip_key(v, key)),
        _ => {}
    }
}

/// The record of an import that wrote `incoming` (stored as `stored`).
fn import_record(photo: &Photo, incoming: Option<&Value>, stored: Option<&Value>) -> ImportRecord {
    ImportRecord {
        source: source_digest(photo),
        translated: edit_digest(incoming),
        stored: edit_digest(stored),
    }
}

/// The other editor's name for the summary line.
fn editor_name(photo: &Photo) -> String {
    let editor = match photo.develop {
        Develop::CaptureOne(_) => "Capture One",
        Develop::LightroomXmp(_) => "Lightroom",
        Develop::None => "other editor's",
    };
    match &photo.copy_name {
        Some(name) => format!("{editor} “{name}”"),
        None => editor.into(),
    }
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
        capture_one_rotation(entries, path, &mut adjustments);
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
    if let Some(params) = capture_one_generic_params(entries, path) {
        map.insert(lens::CORRECTION_MODE.name().into(), json!("manual"));
        map.insert(lens::DISTORTION_PARAMS.name().into(), params);
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
    get("LensIdGuid").filter(|v| !v.trim().is_empty())?;
    if generic_profile(entries).is_some() {
        return None;
    }
    let amounts = (amount("LensGeoAbr"), amount("LensLightFallOff"));
    (amounts.0.is_some() || amounts.1.is_some()).then_some(amounts)
}

/// Rawtonic's polynomial distortion `k1` (model 0, radii in half
/// diagonals) per percent of Capture One's Generic Distortion. Measured
/// on Capture One 16.9.1 renders of two cameras (notes:
/// benchmark-results/g50s-2026-11-23): the Generic profile moves the
/// picture by a pure cubic of the radius, 0.0012 of the half diagonal per
/// percent at r = 1, with no framing zoom (the frame keeps its size).
/// Geometry against Capture One's frames (Hide Distorted Areas off): 0.7–3.4
/// px rms over interior patches of a 7360 px wide frame, 2.5–3.2 px on a
/// 3648 px one (the best of Rawtonic's other distortion controls: 15–66 px).
const CAPTURE_ONE_GENERIC_K1_PER_PERCENT: f64 = -0.0012;

/// Rawtonic's polynomial distortion `k1` per percent of Capture One's
/// "Generic pincushion distortion" profile. Measured on Capture One 16.9.1
/// renders of two cameras at 25/50/75/100 (notes:
/// benchmark-results/g50l-2026-11-24): a point of its frame at radius r
/// (half diagonals of the uncorrected frame) shows the photo at
/// r (1 + 0.02 × amount × r²), the centre at scale 1, the frame cropped so
/// its corners reach the photo's corners. Model 0 with this `k1` at exact
/// zoom 1, which zooms just as far as the border needs, renders it within
/// 0.2–0.6 px rms on a 7360 px wide frame (1.6–2.8 px on a 3648 px one, a
/// constant offset of that photo's decode), the field of view equal to
/// Capture One's crop to 0.03 %.
const CAPTURE_ONE_PINCUSHION_K1_PER_PERCENT: f64 = 0.02;

/// Capture One's Generic Light Falloff as a radial gain: log2 gain =
/// (amount / 100)^`CAPTURE_ONE_FALLOFF_POWER` × L(ρ), L(ρ) = c₁ρ² + c₂ρ⁴ +
/// c₃ρ⁶ with ρ the distance from the centre over half the frame's long side
/// (the same ρ on 3:2 and 4:3 frames; half diagonals fit the held-out
/// camera worse). Fitted on one camera's renders at 50 and 100 (Nikon D800),
/// compared through each scene's own tone relation between the two editors'
/// renders without falloff (notes: benchmark-results/g50l-2026-11-24):
/// residual 0.015–0.017 EV rms; held-out cameras (E-450, two Canons)
/// 0.015–0.04 EV at 50 and 0.03–0.07 EV at 100 within ρ ≤ 1, more in the
/// far corners at 100 (+3.5…+4.5 EV there).
const CAPTURE_ONE_FALLOFF_L: [f64; 3] = [3.0, -1.01, 0.61];
const CAPTURE_ONE_FALLOFF_POWER: f64 = 0.83;

/// The `LensIdGuid` of Capture One's "Generic pincushion distortion"
/// profile (in its lens menu under the recommended lenses): like Generic, it
/// holds no data of the lens; seen on a Nikon NEF of Capture One 16.9.1
/// (notes: benchmark-results/c1r3-2026-11-23).
const CAPTURE_ONE_GENERIC_PINCUSHION_LENS: &str = "A4C7C116-2472-4B0F-9C39-5775CDAD2FF1";

/// Which of Capture One's lens profiles without lens data `entries` use.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum GenericProfile {
    Generic,
    Pincushion,
}

fn generic_profile(entries: &[(String, String)]) -> Option<GenericProfile> {
    let guid = capture_one_entry(entries, "LensIdGuid")?;
    if is_generic_lens(guid) {
        Some(GenericProfile::Generic)
    } else if guid.eq_ignore_ascii_case(CAPTURE_ONE_GENERIC_PINCUSHION_LENS) {
        Some(GenericProfile::Pincushion)
    } else {
        None
    }
}

/// The amount (`curve|amount|…`) of Capture One's setting `key`, above 0,
/// clamped to its slider's 100.
fn capture_one_amount(entries: &[(String, String)], key: &str) -> Option<f64> {
    capture_one_entry(entries, key)?
        .split('|')
        .nth(1)?
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite() && *v > 0.0)
        .map(|v| v.min(100.0))
}

/// Rounded to the hundred-thousandths: no float dust in recipes.
fn rounded(value: f64) -> f64 {
    (value * 1e5).round() / 1e5
}

/// The `k1` of the Distortion above 0 (0–100 %) on one of Capture One's
/// profiles without lens data (the photo then has no lens profile either,
/// as in Capture One): Generic's barrel, the pincushion profile's own.
/// None for other lenses and at 0 (Rawtonic's default correction stays,
/// `capture_one_lens_report`).
fn capture_one_generic_distortion_k1(entries: &[(String, String)]) -> Option<f64> {
    let per_percent = match generic_profile(entries)? {
        GenericProfile::Generic => CAPTURE_ONE_GENERIC_K1_PER_PERCENT,
        GenericProfile::Pincushion => CAPTURE_ONE_PINCUSHION_K1_PER_PERCENT,
    };
    Some(rounded(
        capture_one_amount(entries, "LensGeoAbr")? * per_percent,
    ))
}

/// The lens vignetting terms (`vig_k1..3`: gain 1 / (1 + k₁r² + k₂r⁴ +
/// k₃r⁶), r in half diagonals) of the Generic profile's Light Falloff above
/// 0 on a frame `aspect` (long over short side) wide: the measured gain
/// (`CAPTURE_ONE_FALLOFF_L`) fitted in relative terms (weighted least
/// squares on 65 radii, within 0.03 EV at 3:2 and 4:3 up to amount 50,
/// 0.06–0.1 EV only in the far corners at 100). None for other profiles,
/// at 0 and where the fit would leave the frame unshaded somewhere.
fn capture_one_generic_falloff(entries: &[(String, String)], aspect: f64) -> Option<[f64; 3]> {
    if generic_profile(entries)? != GenericProfile::Generic || !aspect.is_finite() || aspect < 1.0 {
        return None;
    }
    let strength =
        (capture_one_amount(entries, "LensLightFallOff")? / 100.0).powf(CAPTURE_ONE_FALLOFF_POWER);
    // Half diagonal over half the long side.
    let diagonal = (1.0 + 1.0 / (aspect * aspect)).sqrt();
    let [c1, c2, c3] = CAPTURE_ONE_FALLOFF_L;
    let (mut ata, mut atb) = ([[0.0f64; 3]; 3], [0.0f64; 3]);
    for i in 0..=64 {
        let r = f64::from(i) / 64.0;
        let rho2 = (r * diagonal).powi(2);
        let ev = strength * (c1 * rho2 + c2 * rho2 * rho2 + c3 * rho2 * rho2 * rho2);
        let v = (-ev).exp2();
        let r2 = r * r;
        let basis = [r2, r2 * r2, r2 * r2 * r2].map(|b| b / v);
        let target = (v - 1.0) / v;
        for (row, a) in ata.iter_mut().zip(basis) {
            for (cell, b) in row.iter_mut().zip(basis) {
                *cell += a * b;
            }
        }
        for (cell, a) in atb.iter_mut().zip(basis) {
            *cell += a * target;
        }
    }
    let k = solve3(ata, atb)?.map(rounded);
    // Shaded everywhere (the warp leaves a pixel whose factor is not above
    // 0 unshaded): the fit stays well above it at Capture One's 100.
    let factor = |r: f64| {
        let r2 = r * r;
        1.0 + k[0] * r2 + k[1] * r2 * r2 + k[2] * r2 * r2 * r2
    };
    (0..=256)
        .all(|i| factor(f64::from(i) / 256.0) > 1e-3)
        .then_some(k)
}

/// `a x = b` for a 3 × 3 system (Gaussian elimination, partial pivoting).
fn solve3(mut a: [[f64; 3]; 3], mut b: [f64; 3]) -> Option<[f64; 3]> {
    for col in 0..3 {
        let pivot = (col..3).max_by(|&i, &j| a[i][col].abs().total_cmp(&a[j][col].abs()))?;
        if a[pivot][col].abs() < 1e-12 {
            return None;
        }
        a.swap(col, pivot);
        b.swap(col, pivot);
        let (pivot_row, pivot_b) = (a[col], b[col]);
        for (row, rhs) in a.iter_mut().zip(b.iter_mut()).skip(col + 1) {
            let f = row[col] / pivot_row[col];
            for (cell, p) in row.iter_mut().zip(pivot_row).skip(col) {
                *cell -= f * p;
            }
            *rhs -= f * pivot_b;
        }
    }
    let mut x = [0.0; 3];
    for row in (0..3).rev() {
        let sum: f64 = (row + 1..3).map(|c| a[row][c] * x[c]).sum();
        x[row] = (b[row] - sum) / a[row][row];
    }
    x.iter().all(|v| v.is_finite()).then_some(x)
}

/// The photo's frame as Rawtonic renders it before any crop or turn of
/// the photographer's: width and height as shown (its EXIF orientation
/// applied) and that orientation. A RAW's frame (`source_frame`), another
/// file's pixels.
fn photo_frame(path: &Path) -> Option<(f64, f64, u16)> {
    let (w, h, orientation) = if crate::formats::is_raw_file(path) {
        let source = rawler::rawsource::RawSource::new(path).ok()?;
        let (frame, _) = rawtonic_rawdecode::source_frame::frame_and_sensor(
            &source,
            rawtonic_rawdecode::source_frame::VERSION,
        )?;
        let [_, _, w, h] = frame.rect;
        (w as f64, h as f64, frame.orientation)
    } else {
        let (w, h) = image::image_dimensions(path).ok()?;
        let orientation = std::fs::read(path)
            .ok()
            .map(|bytes| crate::image_loader::source_orientation(&bytes, &path.to_string_lossy()))
            .unwrap_or(1);
        (f64::from(w), f64::from(h), orientation)
    };
    if !(w > 0.0 && h > 0.0) {
        return None;
    }
    Some(if (5..=8).contains(&orientation) {
        (h, w, orientation)
    } else {
        (w, h, orientation)
    })
}

/// The photo's frame (`photo_frame`), long over short side.
fn frame_aspect(path: &Path) -> Option<f64> {
    let (w, h, _) = photo_frame(path)?;
    Some(w.max(h) / w.min(h))
}

/// Capture One's rotation of a photo shown with EXIF `orientation`, as
/// Rawtonic's quarter turns (`orientationSteps`, clockwise) and
/// straightening (`rotation`, −45…45°). Capture One's `Rotation` is the
/// whole clockwise rotation, the EXIF orientation's own quarter turns
/// included (C1R7 session: 90 on an EXIF 6 file, 270 on EXIF 8 ones, 5 on
/// an EXIF 1 RAW straightened by 5°, whose render Rawtonic's `rotation` 5
/// matches to 0.001°; notes: benchmark-results/g50l-2026-11-24). None for
/// mirrored orientations (how Capture One counts those is not measured).
fn capture_one_turns(total: f64, orientation: u16) -> Option<(u64, f64)> {
    let own = match orientation {
        1 => 0.0,
        6 => 90.0,
        3 => 180.0,
        8 => 270.0,
        _ => return None,
    };
    let rest = (total - own).rem_euclid(360.0);
    let rest = if rest > 180.0 { rest - 360.0 } else { rest };
    let quarters = (rest / 90.0).round();
    let straighten = rounded(rest - 90.0 * quarters);
    Some(((quarters as i64).rem_euclid(4) as u64, straighten))
}

/// The largest crop of a `width` × `height` frame's own aspect ratio
/// about its centre that the frame straightened by `degrees` fills: the
/// crop Capture One shows for a straightened photo without a crop of its
/// own (C1R7: 6600 × 4405 of a 7360 × 4912 frame at 5°, 6542 × 4366 in
/// Rawtonic's pixels, Capture One framing 0.9 % tighter through its lens
/// profile; this crop 6532 × 4359).
fn straightened_crop(width: f64, height: f64, degrees: f64) -> Value {
    let (sin, cos) = degrees.to_radians().sin_cos();
    let (sin, cos) = (sin.abs(), cos.abs());
    let aspect = width / height;
    let w = (width / (cos + sin / aspect)).min(height / (sin + cos / aspect));
    let h = w / aspect;
    json!({
        "x": ((width - w) / 2.0).max(0.0),
        "y": ((height - h) / 2.0).max(0.0),
        "width": w,
        "height": h,
    })
}

/// Capture One's crop (`left;top;width;height`): `0;0;0;0` or nothing is
/// none.
fn capture_one_has_crop(entries: &[(String, String)]) -> bool {
    capture_one_entry(entries, "Crop").is_some_and(|crop| {
        crop.split(';')
            .any(|v| v.trim().parse::<f64>().is_ok_and(|v| v != 0.0))
    })
}

/// Capture One's rotation (`capture_one_turns`) as Rawtonic's quarter turns
/// and straightening, a straightened photo cropped as Capture One shows it
/// (`straightened_crop`). Capture One's own crop is not read (its numbers
/// are not measured): the straightened photo still gets the crop without
/// empty corners, and the report names the crop
/// (`capture_one_rotation_report`). A recipe that has its own geometry
/// already keeps it.
fn capture_one_rotation(entries: &[(String, String)], path: &Path, adjustments: &mut Value) {
    let Some(total) = capture_one_entry(entries, "Rotation")
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|v| v.is_finite())
    else {
        return;
    };
    let Some(map) = adjustments.as_object_mut() else {
        return;
    };
    if [
        geometry::ROTATION.name(),
        geometry::ORIENTATION_STEPS.name(),
        geometry::CROP.name(),
    ]
    .iter()
    .any(|key| map.contains_key(*key))
    {
        return;
    }
    let Some((width, height, orientation)) = photo_frame(path) else {
        return;
    };
    let Some((steps, straighten)) = capture_one_turns(total, orientation) else {
        return;
    };
    if steps != 0 {
        map.insert(geometry::ORIENTATION_STEPS.name().into(), json!(steps));
    }
    if straighten != 0.0 {
        let (width, height) = if steps % 2 == 1 {
            (height, width)
        } else {
            (width, height)
        };
        map.insert(geometry::ROTATION.name().into(), json!(straighten));
        map.insert(
            geometry::CROP.name().into(),
            straightened_crop(width, height, straighten),
        );
    }
}

/// The import report's lines for Capture One's rotation and crop
/// Rawtonic does not take over: its crop, and a straightening of a photo
/// whose frame cannot be read or whose EXIF orientation is mirrored.
fn capture_one_rotation_report(entries: &[(String, String)], path: &Path) -> Vec<String> {
    let mut out = Vec::new();
    if capture_one_has_crop(entries) {
        out.push("Capture One's crop not carried".to_string());
    }
    // Quarter turns of a photo whose frame cannot be read (offline) are as
    // a rule its EXIF orientation: only a straightening is named, and the
    // frame is read only then.
    let total = capture_one_entry(entries, "Rotation")
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|v| v.is_finite());
    if let Some(total) = total.filter(|t| t.rem_euclid(90.0) != 0.0) {
        let carried = photo_frame(path)
            .is_some_and(|(_, _, orientation)| capture_one_turns(total, orientation).is_some());
        if !carried {
            out.push(format!("Capture One's rotation {total}° not carried"));
        }
    }
    out
}

/// The Light Falloff terms for the photo at `path`
/// (`capture_one_generic_falloff` on its frame's aspect).
fn capture_one_generic_falloff_for(entries: &[(String, String)], path: &Path) -> Option<[f64; 3]> {
    // The frame is read only for a falloff to carry.
    capture_one_amount(entries, "LensLightFallOff")?;
    generic_profile(entries).filter(|p| *p == GenericProfile::Generic)?;
    capture_one_generic_falloff(entries, frame_aspect(path)?)
}

/// Rawtonic's polynomial correction (`lensDistortionParams` model 0, the
/// Lens tool's "Imported correction") for Capture One's profiles without
/// lens data: the Distortion (`capture_one_generic_distortion_k1`) and the
/// Generic profile's Light Falloff (`capture_one_generic_falloff_for`).
/// None when neither is above 0 or carried.
fn capture_one_generic_params(entries: &[(String, String)], path: &Path) -> Option<Value> {
    generic_profile(entries)?;
    let k1 = capture_one_generic_distortion_k1(entries);
    let vignetting = capture_one_generic_falloff_for(entries, path);
    if k1.is_none() && vignetting.is_none() {
        return None;
    }
    let mut params = json!({"model": 0, "k1": k1.unwrap_or(0.0), "k2": 0.0, "k3": 0.0, "zoom": 1.0, "exact_zoom": true});
    if let Some([a, b, c]) = vignetting {
        params["vig_k1"] = json!(a);
        params["vig_k2"] = json!(b);
        params["vig_k3"] = json!(c);
    }
    Some(params)
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

fn capture_one_color_editor_report(entries: &[(String, String)]) -> Option<String> {
    // As in the style converter, the last entry takes precedence.
    let (_, value) = entries
        .iter()
        .rev()
        .find(|(key, _)| key == "ColorCorrections")?;
    crate::costyle::color_editor_is_partial(value)
        .then(|| "Capture One's Color Editor settings could not be fully translated".into())
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
/// not take over as they are: a profile without lens data (Generic or the
/// pincushion profile; the photo keeps Rawtonic's default correction for
/// its lens, never a lens profile picked in its place, unless a setting on
/// it came over as the polynomial correction,
/// `capture_one_generic_params`) with the amounts set on it that have no
/// measured equivalent (or, for the falloff, no frame to fit it to); and an
/// analysed chromatic aberration curve (Rawtonic measures the photo's own
/// CA instead).
fn capture_one_lens_report(entries: &[(String, String)], path: &Path) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(profile) = generic_profile(entries) {
        let name = match profile {
            GenericProfile::Generic => "Generic",
            GenericProfile::Pincushion => "Generic pincushion",
        };
        // A setting that came over as the polynomial is no setting left out.
        let distortion_carried = capture_one_generic_distortion_k1(entries).is_some();
        let falloff_carried = capture_one_generic_falloff_for(entries, path).is_some();
        if !distortion_carried && !falloff_carried {
            out.push(format!(
                "Capture One's {name} lens profile: Rawtonic's default lens correction for this lens applies"
            ));
        }
        for (key, setting, carried) in [
            ("LensGeoAbr", "Distortion", distortion_carried),
            ("LensLightFallOff", "Light Falloff", falloff_carried),
        ] {
            if carried {
                continue;
            }
            let amount = capture_one_entry(entries, key)
                .and_then(|v| v.split('|').nth(1))
                .and_then(|v| v.trim().parse::<f64>().ok())
                .filter(|v| v.is_finite() && *v != 0.0);
            if let Some(amount) = amount {
                out.push(format!(
                    "Capture One's {setting} {amount} with the {name} profile not carried (no measured equivalent)"
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
        ..Default::default()
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
    let own_adjustments = |id: &str| -> anyhow::Result<Option<Value>> {
        Ok(catalog
            .editor_state(id)?
            .and_then(|state| state.get("adjustments").cloned()))
    };
    let (id, rule, before) = match catalog.asset_at(&photo.path)? {
        Some(asset) => {
            let mut contents = contents;
            let own = own_adjustments(&asset.id)?;
            let version = catalog.version_for(&asset.id, None)?;
            let record = match &version {
                Some(version) => catalog.imported_recipe(version)?,
                None => None,
            };
            let settled = settle_rule(
                record,
                own.as_ref(),
                sidecar_edit(&asset.path, None, own.as_ref()),
                photo,
                adjustments.as_ref(),
            );
            if let (Some(version), Some(upgrade)) = (&version, &settled.upgrade) {
                catalog.record_imported_recipe(version, upgrade)?;
            }
            if settled.unread_straightening {
                let path = asset.path.to_string_lossy().into_owned();
                imported.warning = Some(unread_straightening_line(&path, adjustments.as_ref()));
                imported.geometry_offer = Some(GeometryOffer::of(path, adjustments.as_ref()));
            }
            let rule = settled.rule;
            if rule != Reimport::Apply {
                contents.editor_state = None;
                imported.translated = false;
                imported.kept_with_newer = rule == Reimport::KeepWithNewer;
            }
            catalog.apply_contents(&asset.id, &contents)?;
            (asset.id, rule, Some(own))
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
            (id, Reimport::Apply, None)
        }
    };
    if let Some(lens) = &photo.lens {
        catalog.set_imported_lens(&id, lens)?;
    }
    if let rawtonic_metadata::CaptureWrite::Set(value) = &photo.metadata.capture {
        if let Some(facts) = rawtonic_ingest::capture::capture_facts(&photo.path) {
            catalog.record_facts(&id, &facts)?;
        }
        catalog.import_wall_capture_time(&id, value)?;
    }
    let version = catalog.version_for(&id, None)?;
    if matches!(rule, Reimport::Apply | Reimport::Same)
        && let Some(version) = &version
    {
        let stored = own_adjustments(&id)?;
        catalog.record_imported_recipe(
            version,
            &import_record(photo, adjustments.as_ref(), stored.as_ref()),
        )?;
        // A photograph here before whose edit the import changed: Undo
        // puts it back.
        if rule == Reimport::Apply
            && let Some(before) = before
            && edit_digest(before.as_ref()) != edit_digest(stored.as_ref())
        {
            imported.change = Some(RecipeChange {
                path: catalog.asset(&id)?.path.to_string_lossy().into_owned(),
                before: before.unwrap_or(Value::Null),
                after: stored.unwrap_or(Value::Null),
            });
        }
    }
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
            let kept = reimport_copy(catalog, asset, &candidate, photo)?;
            // Its derived sidecar is published again if it is missing (an
            // earlier import could not write beside the photograph), so
            // listings find the copy.
            let sidecar = sidecar_of(&candidate);
            if photo.path.is_file()
                && !sidecar.exists()
                && let Some(state) = catalog.copy_state(asset, &candidate)?
            {
                publish_new(&sidecar, &serde_json::to_vec_pretty(&state)?)?;
            }
            return Ok((kept, candidate));
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
        ..Default::default()
    };
    let incoming = recipe.as_ref().map(|(adjustments, _)| adjustments.clone());
    let mut state = json!({
        "version": crate::image_processing::ImageMetadata::default().version,
        "rating": photo.metadata.rating,
        "tags": [],
        "adjustments": recipe.map_or(Value::Null, |(adjustments, _)| adjustments),
    });
    rawtonic_catalog::apply_editor_metadata(&mut state, photo.metadata.clone());
    catalog.save_copy_state(asset, &id, &state)?;
    // The copy's own pick flag (Lightroom keeps one per copy).
    catalog.set_item_flags(&[(asset.to_string(), Some(id.clone()), photo.metadata.flag)])?;
    let version = catalog.version_for(asset, Some(&id))?;
    if let Some(version) = &version {
        let stored = catalog
            .copy_state(asset, &id)?
            .and_then(|state| state.get("adjustments").cloned());
        catalog.record_imported_recipe(
            version,
            &import_record(photo, incoming.as_ref(), stored.as_ref()),
        )?;
    }
    imported.crops_clipped += import_snapshots(catalog, version.as_deref(), photo)?;
    if photo.path.is_file()
        && let Some(state) = catalog.copy_state(asset, &id)?
    {
        publish_new(&sidecar_of(&id), &serde_json::to_vec_pretty(&state)?)?;
    }
    Ok((imported, id))
}

/// A repeated import of the copy `copy` of `asset`, which an earlier import
/// made: the rule of `reimport` for its edit. Brought up to date, it keeps
/// its metadata here (rating, keywords, label); its derived sidecar follows
/// after the import (`session_bridge::rederive`).
fn reimport_copy(
    catalog: &Library,
    asset: &str,
    copy: &str,
    photo: &Photo,
) -> anyhow::Result<Imported> {
    let mut imported = Imported::default();
    let Some(version) = catalog.version_for(asset, Some(copy))? else {
        return Ok(imported);
    };
    let state = catalog.copy_state(asset, copy)?;
    let own = state
        .as_ref()
        .and_then(|state| state.get("adjustments").cloned());
    let recipe = recipe_for(&photo.develop, &photo.capture_one_layers, &photo.path);
    let incoming = recipe.as_ref().map(|(adjustments, _)| adjustments.clone());
    let settled = settle_rule(
        catalog.imported_recipe(&version)?,
        own.as_ref(),
        sidecar_edit(&catalog.asset(asset)?.path, Some(copy), own.as_ref()),
        photo,
        incoming.as_ref(),
    );
    if let Some(upgrade) = &settled.upgrade {
        catalog.record_imported_recipe(&version, upgrade)?;
    }
    if settled.unread_straightening {
        let path = photo_ref::PhotoRef::of_path(&catalog.asset(asset)?.path, Some(copy)).format();
        imported.warning = Some(unread_straightening_line(&path, incoming.as_ref()));
        imported.geometry_offer = Some(GeometryOffer::of(path, incoming.as_ref()));
    }
    let rule = settled.rule;
    match rule {
        Reimport::Apply => {
            let previous = state.clone();
            if let (Some(mut state), Some((adjustments, clipped))) = (state, recipe) {
                imported.translated = true;
                imported.ai_masks = has_ai_masks_on_open(&adjustments);
                imported.crops_clipped = usize::from(clipped);
                state["adjustments"] = adjustments;
                catalog.save_copy_state(asset, copy, &state)?;
            }
            let stored = catalog
                .copy_state(asset, copy)?
                .and_then(|state| state.get("adjustments").cloned());
            if edit_digest(own.as_ref()) != edit_digest(stored.as_ref()) {
                let change = RecipeChange {
                    path: photo_ref::PhotoRef::of_path(&catalog.asset(asset)?.path, Some(copy))
                        .format(),
                    before: own.unwrap_or(Value::Null),
                    after: stored.clone().unwrap_or(Value::Null),
                };
                // The editor reads an online copy from its sidecar: the
                // update counts only once that file holds it. Otherwise the
                // library's copy goes back as it was, unrecorded, so the next
                // import tries again, and it is no part of the Undo step.
                let failed = match follow_sidecar(catalog, asset, &change) {
                    Ok(true) => None,
                    Ok(false) => Some(changed_elsewhere(&change)),
                    Err(e) => Some(format!(
                        "{}: {e}; this virtual copy was not updated",
                        photo_ref::PhotoRef::parse(&change.path)
                            .sidecar_path()
                            .display()
                    )),
                };
                if let Some(error) = failed {
                    if let Some(previous) = &previous {
                        catalog.save_copy_state(asset, copy, previous)?;
                    }
                    imported.translated = false;
                    imported.error = Some(error);
                    return Ok(imported);
                }
                imported.change = Some(change);
            }
            catalog.record_imported_recipe(
                &version,
                &import_record(photo, incoming.as_ref(), stored.as_ref()),
            )?;
        }
        Reimport::Same => {
            catalog.record_imported_recipe(
                &version,
                &import_record(photo, incoming.as_ref(), own.as_ref()),
            )?;
        }
        Reimport::Keep => {}
        Reimport::KeepWithNewer => imported.kept_with_newer = true,
    }
    Ok(imported)
}

/// The other editor's changed settings for a photograph or copy whose edit
/// here was kept (`Reimport::KeepWithNewer`), as a new virtual copy of
/// `asset` with the photo's metadata, beside the kept edit: its id is
/// derived from the source's identity and those settings, so a repeated
/// import of the same settings finds it instead of making another. Returns
/// the summary line when it was made.
fn import_newer_copy(
    catalog: &Library,
    asset: &str,
    source_name: &str,
    photo: &Photo,
    of_copy: Option<&str>,
) -> anyhow::Result<Option<String>> {
    let origin = format!(
        "newer\0{}\0{}",
        copy_origin(source_name, photo),
        source_digest(photo)
    );
    let known = catalog.copies(asset)?;
    let mut id = None;
    for attempt in 0..16 {
        let candidate = copy_id(asset, &origin, attempt);
        let sidecar = photo
            .path
            .with_file_name(photo_ref::sidecar_name(&photo.path, Some(&candidate)));
        if known.contains(&candidate) {
            // Made before; its derived sidecar again if an earlier import
            // could not write it, so listings find the copy.
            if photo.path.is_file()
                && !sidecar.exists()
                && let Some(state) = catalog.copy_state(asset, &candidate)?
            {
                publish_new(&sidecar, &serde_json::to_vec_pretty(&state)?)?;
            }
            return Ok(None);
        }
        if !sidecar.exists() {
            id = Some((candidate, sidecar));
            break;
        }
    }
    let (id, sidecar) = id.ok_or_else(|| anyhow::anyhow!("No free name for the virtual copy"))?;
    let Some((adjustments, _)) = recipe_for(&photo.develop, &photo.capture_one_layers, &photo.path)
    else {
        return Ok(None);
    };
    let mut state = json!({
        "version": crate::image_processing::ImageMetadata::default().version,
        "rating": photo.metadata.rating,
        "tags": [],
        "adjustments": adjustments,
    });
    rawtonic_catalog::apply_editor_metadata(&mut state, photo.metadata.clone());
    catalog.save_copy_state(asset, &id, &state)?;
    if photo.path.is_file()
        && let Some(state) = catalog.copy_state(asset, &id)?
    {
        publish_new(&sidecar, &serde_json::to_vec_pretty(&state)?)?;
    }
    let what = match of_copy {
        Some(copy) => format!("{} (virtual copy {copy})", photo.path.display()),
        None => photo.path.display().to_string(),
    };
    Ok(Some(format!(
        "{what}: kept your edit; the newer {} settings are in a new virtual copy",
        editor_name(photo)
    )))
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

/// What "Apply Capture One straightening and crop" did.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GeometryApplied {
    /// Each changed edit's whole recipe before and after: one Undo step.
    pub recipe_changes: Vec<RecipeChange>,
    /// Offered paths the source has no photograph for, or that already
    /// follow Capture One's geometry (nothing written).
    pub unchanged: Vec<String>,
    pub errors: Vec<String>,
}

/// The recipe keys Capture One's straightening and crop set
/// (`capture_one_rotation`): the only ones the action replaces.
const SOURCE_GEOMETRY_KEYS: [&str; 3] = [
    geometry::ROTATION.name(),
    geometry::ORIENTATION_STEPS.name(),
    geometry::CROP.name(),
];

/// The photograph's or virtual copy's `paths` (as `ImportReport::geometry_offers`
/// names them) take over the straightening, quarter turns and crop of the
/// source's translation; every other adjustment stays. One `RecipeChange`
/// each, for one Undo step; the record of the last import is brought to
/// today's reader, so the repeated import no longer names the photo. Only
/// the library's edit and its derived sidecar are written, never the photo.
pub fn apply_source_geometry(
    catalog: &Library,
    source: &Source,
    offers: &[GeometryOffer],
) -> GeometryApplied {
    let mut applied = GeometryApplied::default();
    for photo in &source.photos {
        let parent = photo
            .copy_of
            .as_deref()
            .and_then(|key| source.photos.iter().find(|p| p.key == key))
            .unwrap_or(photo);
        let Ok(Some(asset)) = catalog.asset_at(&parent.path) else {
            continue;
        };
        let copy = if photo.copy_of.is_some() {
            let origin = copy_origin(&source.name, photo);
            let known = catalog.copies(&asset.id).unwrap_or_default();
            match (0..16)
                .map(|attempt| copy_id(&asset.id, &origin, attempt))
                .find(|id| known.contains(id))
            {
                Some(id) => Some(id),
                None => continue,
            }
        } else {
            None
        };
        let path = match &copy {
            Some(id) => photo_ref::PhotoRef::of_path(&asset.path, Some(id)).format(),
            None => asset.path.to_string_lossy().into_owned(),
        };
        let Some(offer) = offers.iter().find(|o| o.path == path) else {
            continue;
        };
        match apply_geometry_to(catalog, &asset.id, copy.as_deref(), &path, photo, offer) {
            Ok(Some(change)) => {
                let followed = follow_sidecar(catalog, &asset.id, &change);
                // Not written beside the photo (changed there since, or the
                // file cannot be written): a virtual copy is read from its
                // sidecar, so the library's edit goes back as it was and
                // nothing counts as applied, as in a repeated import. A
                // photograph's own edit stays in the library, as after an
                // edit whose sidecar could not be written.
                let back = match &followed {
                    Ok(false) => true,
                    Err(_) => copy.is_some(),
                    Ok(true) => false,
                };
                if back && let Err(e) = put_back(catalog, &asset.id, copy.as_deref(), &change) {
                    applied.errors.push(format!("{path}: {e:#}"));
                }
                match followed {
                    Ok(true) => {
                        record_current(catalog, &asset.id, copy.as_deref(), photo);
                        applied.recipe_changes.push(change);
                    }
                    Ok(false) => applied.errors.push(changed_elsewhere(&change)),
                    Err(e) => {
                        applied.errors.push(format!("{path}: {e}"));
                        if copy.is_none() {
                            record_current(catalog, &asset.id, None, photo);
                            applied.recipe_changes.push(change);
                        }
                    }
                }
            }
            Ok(None) => applied.unchanged.push(path),
            Err(e) => applied.errors.push(format!("{path}: {e:#}")),
        }
    }
    applied
}

/// The edit of `change.before` back in the library.
fn put_back(
    catalog: &Library,
    asset: &str,
    copy: Option<&str>,
    change: &RecipeChange,
) -> anyhow::Result<()> {
    let state = match copy {
        Some(id) => catalog.copy_state(asset, id)?,
        None => catalog.editor_state(asset)?,
    };
    let Some(mut state) = state else {
        return Ok(());
    };
    state["adjustments"] = change.before.clone();
    match copy {
        Some(id) => catalog.save_copy_state(asset, id, &state)?,
        None => catalog.save_editor_state(asset, &state)?,
    }
    Ok(())
}

/// The version's import record as of today's reader (the repeated import
/// then no longer names the straightening). A failure leaves the record
/// as it was: the import names the photo again.
fn record_current(catalog: &Library, asset: &str, copy: Option<&str>, photo: &Photo) {
    let _ = (|| -> anyhow::Result<()> {
        if let Some(version) = catalog.version_for(asset, copy)? {
            let (current, _) = current_record(catalog.imported_recipe(&version)?, photo);
            if let Some(current) = current {
                catalog.record_imported_recipe(&version, &current)?;
            }
        }
        Ok(())
    })();
}

/// The geometry step of `apply_source_geometry` for one version: the change
/// made, none when it follows the source's geometry already.
fn apply_geometry_to(
    catalog: &Library,
    asset: &str,
    copy: Option<&str>,
    path: &str,
    photo: &Photo,
    offer: &GeometryOffer,
) -> anyhow::Result<Option<RecipeChange>> {
    let state = match copy {
        Some(id) => catalog.copy_state(asset, id)?,
        None => catalog.editor_state(asset)?,
    };
    let Some(mut state) = state else {
        anyhow::bail!("no edit here to apply it to");
    };
    // The geometry is read from the photo's frame: without it (the file is
    // offline or unreadable) the translation holds none, which must not
    // clear what is saved.
    let Some((width, height, _)) = photo_frame(&photo.path) else {
        anyhow::bail!(
            "the photo's frame cannot be read (is the file online?); nothing was changed"
        );
    };
    let Some((incoming, _)) = recipe_for(&photo.develop, &photo.capture_one_layers, &photo.path)
    else {
        return Ok(None);
    };
    // Offered for a photo Capture One straightens: where it does not any
    // more (reset there since), the saved geometry is not replaced.
    if !straightens(Some(&incoming)) {
        anyhow::bail!("Capture One no longer straightens or turns this photo; nothing was changed");
    }
    // What the person read is what is applied: Capture One's session changed
    // since (the report's angle is no longer its angle) writes nothing and
    // names the new value.
    if straightening_differs(Some(&offer.as_recipe()), Some(&incoming)) {
        let now = GeometryOffer::of(path.to_string(), Some(&incoming));
        anyhow::bail!(
            "Capture One now shows {}° (turned {} quarter turn(s)), not {}° (turned {}) as shown; nothing was changed. Import again to see the current value",
            now.rotation,
            now.orientation_steps,
            offer.rotation,
            offer.orientation_steps
        );
    }
    let before = state.get("adjustments").cloned().unwrap_or(Value::Null);
    // The crop is made on the file's newest frame: an edit that develops an
    // older one (`sourceFrameVersion`, a Sony ARW before the whole sensor)
    // would show it beside another frame.
    if crate::formats::is_raw_file(&photo.path)
        && let Ok(source) = rawler::rawsource::RawSource::new(&photo.path)
    {
        let saved = before
            .get("sourceFrameVersion")
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32;
        let frame = rawtonic_rawdecode::source_frame::effective;
        if frame(&source, saved) != frame(&source, rawtonic_rawdecode::source_frame::VERSION) {
            anyhow::bail!(
                "this edit develops an older frame of the file than the one Capture One's geometry is made for; nothing was changed"
            );
        }
    }
    let mut after = if before.is_object() {
        before.clone()
    } else {
        json!({})
    };
    let map = after.as_object_mut().expect("an object");
    for key in SOURCE_GEOMETRY_KEYS {
        match incoming.get(key) {
            Some(value) => map.insert(key.into(), value.clone()),
            None => map.remove(key),
        };
    }
    if edit_digest(Some(&before)) == edit_digest(Some(&after)) {
        return Ok(None);
    }
    // Drawn masks without a frame stay on the photo's content.
    crate::mask_generation::keep_drawn_masks_on_photo(&before, &mut after, Some((width, height)));
    state["adjustments"] = after.clone();
    match copy {
        Some(id) => catalog.save_copy_state(asset, id, &state)?,
        None => catalog.save_editor_state(asset, &state)?,
    }
    // The stored edit as the library holds it, which Undo compares with.
    let stored = match copy {
        Some(id) => catalog.copy_state(asset, id)?,
        None => catalog.editor_state(asset)?,
    }
    .and_then(|state| state.get("adjustments").cloned())
    .unwrap_or(after);
    Ok(Some(RecipeChange {
        path: path.to_string(),
        before,
        after: stored,
    }))
}

#[tauri::command]
pub async fn library_apply_source_geometry(
    source: String,
    directory: String,
    relinks: Vec<Relink>,
    offers: Vec<GeometryOffer>,
    app_handle: tauri::AppHandle,
) -> Result<GeometryApplied, String> {
    let applied = tauri::async_runtime::spawn_blocking(move || {
        let (mut lib, _) = read_source(Path::new(&source))?;
        for relink in &relinks {
            lib.relink(&relink.from, &relink.to);
        }
        let catalog = Library::open(Path::new(&directory)).map_err(|e| e.to_string())?;
        Ok::<_, String>(apply_source_geometry(&catalog, &lib, &offers))
    })
    .await
    .map_err(|e| e.to_string())??;
    crate::file_management::refresh_thumbnails(
        app_handle,
        applied
            .recipe_changes
            .iter()
            .map(|change| change.path.clone())
            .collect(),
    );
    Ok(applied)
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
    app_handle: tauri::AppHandle,
) -> Result<ImportReport, String> {
    let report = tauri::async_runtime::spawn_blocking(move || {
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
    .map_err(|e| e.to_string())??;
    // Tiles of edits brought up to date show them.
    crate::file_management::refresh_thumbnails(
        app_handle,
        report
            .recipe_changes
            .iter()
            .map(|change| change.path.clone())
            .collect(),
    );
    Ok(report)
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

    /// G50L: the Generic profile's Light Falloff comes over as the lens
    /// vignetting terms of the polynomial correction, fitted to the gain
    /// measured on Capture One's renders for the photo's frame; with its
    /// Distortion in the same correction; not where the frame cannot be read
    /// (reported as before).
    #[test]
    fn capture_one_generic_light_falloff_comes_over_as_lens_vignetting() {
        let entries = |pairs: &[(&str, &str)]| -> Vec<(String, String)> {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        };
        let dir = tempfile::tempdir().unwrap();
        // A 3:2 frame, as Rawtonic renders it.
        let photo_path = dir.path().join("a.png");
        image::RgbImage::new(300, 200).save(&photo_path).unwrap();
        let falloff = |amount: &str, distortion: &str| {
            entries(&[
                ("LensIdGuid", CAPTURE_ONE_GENERIC_LENS),
                ("LensGeoAbr", distortion),
                ("LensLightFallOff", amount),
            ])
        };
        // log2 gain the measured model gives at `rho` (half the long side).
        let measured = |amount: f64, rho: f64| {
            let [c1, c2, c3] = CAPTURE_ONE_FALLOFF_L;
            let x = rho * rho;
            (amount / 100.0).powf(CAPTURE_ONE_FALLOFF_POWER)
                * (c1 * x + c2 * x * x + c3 * x * x * x)
        };
        for amount in [10.0, 50.0, 100.0] {
            let c1 = falloff(&format!("|{amount}|1"), "|0.000000|1|100|-1");
            let mut adjustments = json!({});
            capture_one_lens(&c1, &photo_path, &mut adjustments);
            assert_eq!(adjustments["lensCorrectionMode"], "manual");
            let params = &adjustments["lensDistortionParams"];
            assert_eq!(params["model"], 0);
            assert_eq!(params["k1"], 0.0, "Distortion 0: the photo is not moved");
            assert_eq!(params["exact_zoom"], true);
            let k = ["vig_k1", "vig_k2", "vig_k3"].map(|key| params[key].as_f64().unwrap());
            // The renderer's gain 1 / (1 + k1 r² + k2 r⁴ + k3 r⁶), r in half
            // diagonals (1.2019 half long sides on 3:2), against the model.
            let diagonal = (1.0f64 + 1.0 / 2.25).sqrt();
            for i in 0..=20 {
                let r = f64::from(i) / 20.0;
                let r2 = r * r;
                let ours = -(1.0 + k[0] * r2 + k[1] * r2 * r2 + k[2] * r2 * r2 * r2).log2();
                let want = measured(amount, r * diagonal);
                let tolerance = if r * diagonal <= 1.0 { 0.06 } else { 0.1 };
                assert!(
                    (ours - want).abs() < tolerance,
                    "amount {amount} r {r}: {ours} vs {want}"
                );
            }
            assert!(
                capture_one_lens_report(&c1, &photo_path).is_empty(),
                "{:?}",
                capture_one_lens_report(&c1, &photo_path)
            );
        }
        // Distortion and Light Falloff together: one correction.
        let both = falloff("|50|1", "|30.0|1|100|-1");
        let mut adjustments = json!({});
        capture_one_lens(&both, &photo_path, &mut adjustments);
        assert_eq!(adjustments["lensDistortionParams"]["k1"], json!(-0.036));
        assert!(
            adjustments["lensDistortionParams"]["vig_k1"]
                .as_f64()
                .unwrap()
                < -0.5
        );
        assert!(capture_one_lens_report(&both, &photo_path).is_empty());
        // A frame that cannot be read: the falloff is not guessed.
        let unreadable = dir.path().join("b.ORF");
        fs::write(&unreadable, b"not a raw").unwrap();
        let mut adjustments = json!({});
        capture_one_lens(&falloff("|50|1", "|0|1"), &unreadable, &mut adjustments);
        assert_eq!(adjustments, json!({}));
        assert_eq!(
            capture_one_lens_report(&falloff("|50|1", "|0|1"), &unreadable),
            [
                "Capture One's Generic lens profile: Rawtonic's default lens correction for this lens applies",
                "Capture One's Light Falloff 50 with the Generic profile not carried (no measured equivalent)"
            ]
        );
    }

    /// G50L (C1R7 session): Capture One's `Rotation` is the photo's whole
    /// clockwise rotation, its EXIF orientation included. Its own quarter
    /// turns come over as `orientationSteps`, the straightening as
    /// `rotation` with the crop Capture One shows (the largest of the
    /// frame's aspect ratio), the EXIF orientation's part not twice; its crop
    /// is named in the report, not guessed.
    #[test]
    fn capture_one_straightening_and_quarter_turns_come_over() {
        assert_eq!(capture_one_turns(5.0, 1), Some((0, 5.0)));
        assert_eq!(capture_one_turns(90.0, 6), Some((0, 0.0)));
        assert_eq!(capture_one_turns(270.0, 8), Some((0, 0.0)));
        assert_eq!(capture_one_turns(180.0, 3), Some((0, 0.0)));
        assert_eq!(capture_one_turns(95.0, 1), Some((1, 5.0)));
        assert_eq!(capture_one_turns(93.0, 6), Some((0, 3.0)));
        assert_eq!(capture_one_turns(357.0, 1), Some((0, -3.0)));
        assert_eq!(capture_one_turns(-3.0, 1), Some((0, -3.0)));
        assert_eq!(capture_one_turns(88.0, 1), Some((1, -2.0)));
        assert_eq!(capture_one_turns(0.0, 6), Some((3, 0.0)));
        assert_eq!(capture_one_turns(90.0, 2), None);

        // The 36 MP frame Capture One straightened by 5°: 6542 × 4366 of
        // Rawtonic's pixels in Capture One's render (its lens framing 0.9 %
        // tighter), the largest crop of the frame's aspect ratio 6532 × 4359.
        let crop = straightened_crop(7360.0, 4912.0, 5.0);
        let (w, h) = (
            crop["width"].as_f64().unwrap(),
            crop["height"].as_f64().unwrap(),
        );
        assert!(
            (w - 6532.0).abs() < 1.0 && (h - 4359.0).abs() < 1.0,
            "{crop}"
        );
        assert!((crop["x"].as_f64().unwrap() - (7360.0 - w) / 2.0).abs() < 1e-9);
        // Every corner of the crop shows the photo (inside the frame
        // turned back by the straightening).
        let (sin, cos) = 5f64.to_radians().sin_cos();
        for (sx, sy) in [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
            let (x, y) = (sx * w / 2.0, sy * h / 2.0);
            let (u, v) = (x * cos + y * sin, -x * sin + y * cos);
            assert!(u.abs() <= 3680.0 + 1e-6 && v.abs() <= 2456.0 + 1e-6);
        }

        let entries = |pairs: &[(&str, &str)]| -> Vec<(String, String)> {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        };
        let dir = tempfile::tempdir().unwrap();
        let photo_path = dir.path().join("a.png");
        image::RgbImage::new(300, 200).save(&photo_path).unwrap();
        let rotated = |rotation: &str| {
            let mut adjustments = json!({});
            capture_one_rotation(
                &entries(&[
                    ("Rotation", rotation),
                    ("Crop", "0.000000;0.000000;0.000000;0.000000"),
                ]),
                &photo_path,
                &mut adjustments,
            );
            adjustments
        };
        assert_eq!(
            rotated("5"),
            json!({"rotation": 5.0, "crop": straightened_crop(300.0, 200.0, 5.0)})
        );
        assert_eq!(rotated("0"), json!({}));
        // A quarter turn of the photographer's: the frame turns, nothing is
        // straightened or cropped.
        assert_eq!(rotated("90"), json!({"orientationSteps": 1}));
        // Turned and straightened: the crop is of the turned frame.
        assert_eq!(
            rotated("265"),
            json!({"orientationSteps": 3, "rotation": -5.0,
                   "crop": straightened_crop(200.0, 300.0, -5.0)})
        );
        // Through the whole recipe, a rotation alone is an edit.
        let (recipe, _) = recipe_for(
            &Develop::CaptureOne(entries(&[("Rotation", "5")])),
            &[],
            &photo_path,
        )
        .unwrap();
        assert_eq!(recipe["rotation"], json!(5.0));
        // Capture One's own crop is named, not guessed.
        assert!(
            capture_one_rotation_report(&entries(&[("Rotation", "5")]), &photo_path).is_empty()
        );
        assert_eq!(
            capture_one_rotation_report(
                &entries(&[("Rotation", "0"), ("Crop", "120;80;2000;1500")]),
                &photo_path
            ),
            ["Capture One's crop not carried"]
        );
        // A frame that cannot be read: not guessed, reported.
        let unreadable = dir.path().join("b.NEF");
        fs::write(&unreadable, b"not a raw").unwrap();
        let mut adjustments = json!({});
        capture_one_rotation(
            &entries(&[("Rotation", "5")]),
            &unreadable,
            &mut adjustments,
        );
        assert_eq!(adjustments, json!({}));
        assert_eq!(
            capture_one_rotation_report(&entries(&[("Rotation", "5")]), &unreadable),
            ["Capture One's rotation 5° not carried"]
        );
    }

    /// KWC1: the source's keyword library arrives whole: terms no photograph
    /// holds are kept in the Keyword List (declared), names holding `;`, `,`
    /// or `>` as one keyword, and a repeated import adds nothing.
    #[test]
    fn import_keeps_the_keyword_library_with_unused_terms() {
        let tmp = tempfile::tempdir().unwrap();
        let catalog = Library::create(&tmp.path().join("Rawtonic"), "Main", Mode::Catalog).unwrap();
        let file = tmp.path().join("a.png");
        image::RgbImage::new(30, 20).save(&file).unwrap();
        let mut held = photo("1", file.clone(), Develop::None, None);
        held.metadata.keywords = vec!["semi;colon".into()];
        let source = Source {
            name: "C1".into(),
            photos: vec![held],
            albums: vec![],
            stacks: vec![],
            keywords: vec![
                "angle|term".into(),
                "angle>term".into(),
                "Parent|comma,term".into(),
                "semi;colon".into(),
                "unused_alpha".into(),
            ],
            not_read: NotRead::default(),
        };
        let report = import_into(&catalog, &source).unwrap();
        assert_eq!((report.imported, report.keywords), (1, 5), "{report:?}");
        assert_eq!(
            catalog.declared_keywords().unwrap(),
            [
                "angle>term",
                "angle|term",
                "Parent|comma,term",
                "semi;colon",
                "unused_alpha"
            ]
        );
        let again = import_into(&catalog, &source).unwrap();
        assert_eq!(again.keywords, 0);
        assert_eq!(catalog.declared_keywords().unwrap().len(), 5);
    }

    /// C1G1: "Apply Capture One straightening and crop" for a photo an
    /// earlier build imported (its record cannot tell whether the
    /// straightening is new): the import offers it, the action replaces only
    /// the straightening, quarter turns and crop (one change for one Undo
    /// step), the photo, the other adjustments and the digest of its edit
    /// stay, and a repeated import names it no more.
    #[test]
    fn apply_source_geometry_replaces_only_the_geometry_and_clears_the_warning() {
        let tmp = tempfile::tempdir().unwrap();
        let catalog = Library::create(&tmp.path().join("Rawtonic"), "Main", Mode::Catalog).unwrap();
        let photos = tmp.path().join("photos");
        fs::create_dir_all(&photos).unwrap();
        let file = photos.join("a.png");
        image::RgbImage::new(300, 200).save(&file).unwrap();
        let original = fs::read(&file).unwrap();
        let entries = |pairs: &[(&str, &str)]| -> Vec<(String, String)> {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        };
        let source = |pairs: &[(&str, &str)]| Source {
            name: "C1".into(),
            photos: vec![photo(
                "1",
                file.clone(),
                Develop::CaptureOne(entries(pairs)),
                None,
            )],
            albums: vec![],
            stacks: vec![],
            keywords: Vec::new(),
            not_read: NotRead::default(),
        };
        // Imported by an earlier build: it read neither Rotation nor Crop.
        import_into(&catalog, &source(&[("Exposure", "0.3")])).unwrap();
        let asset = catalog.asset_at(&file).unwrap().unwrap();
        let mut state = catalog.editor_state(&asset.id).unwrap().unwrap();
        state["adjustments"]["exposure"] = json!(0.9);
        state["adjustments"]["contrast"] = json!(12);
        // A drawn mask without a frame (made before frames were kept).
        state["adjustments"]["masks"] = json!([{"id": "m", "visible": true, "subMasks": [
            {"id": "s", "type": "radial", "visible": true, "mode": "additive",
             "parameters": {"centerX": 100.0, "centerY": 60.0, "radiusX": 40.0,
                            "radiusY": 40.0, "rotation": 0.0, "feather": 0.5}}]}]);
        catalog.save_editor_state(&asset.id, &state).unwrap();
        let own = catalog.editor_state(&asset.id).unwrap().unwrap()["adjustments"].clone();

        // Capture One straightens it by 5°: kept, named, offered.
        let level = source(&[("Exposure", "0.3"), ("Rotation", "5"), ("Crop", "0;0;0;0")]);
        let report = import_into(&catalog, &level).unwrap();
        let path = asset.path.to_string_lossy().into_owned();
        assert_eq!(
            report
                .geometry_offers
                .iter()
                .map(|o| &o.path)
                .collect::<Vec<_>>(),
            [&path],
            "{report:?}"
        );
        assert_eq!(report.warnings.len(), 1, "{report:?}");
        assert!(report.recipe_changes.is_empty());

        // The report shows what is applied.
        let offer = report.geometry_offers.clone();
        assert_eq!(report.geometry_offers[0].rotation, 5.0);
        // Capture One's session changed since the report (10° now): nothing
        // is written, the new value is named.
        let tenfold = source(&[("Exposure", "0.3"), ("Rotation", "10"), ("Crop", "0;0;0;0")]);
        let stale = apply_source_geometry(&catalog, &tenfold, &report.geometry_offers);
        assert!(stale.recipe_changes.is_empty(), "{stale:?}");
        assert!(
            stale.errors[0].contains("now shows 10°") && stale.errors[0].contains("not 5°"),
            "{stale:?}"
        );
        assert_eq!(
            catalog.editor_state(&asset.id).unwrap().unwrap()["adjustments"],
            own
        );
        let applied = apply_source_geometry(&catalog, &level, &report.geometry_offers);
        assert!(applied.errors.is_empty(), "{applied:?}");
        assert_eq!(applied.recipe_changes.len(), 1, "{applied:?}");
        let change = &applied.recipe_changes[0];
        assert_eq!(change.path, path);
        // One Undo step: the whole recipe from before comes back.
        assert_eq!(edit_digest(Some(&change.before)), edit_digest(Some(&own)));
        let now = catalog.editor_state(&asset.id).unwrap().unwrap()["adjustments"].clone();
        assert_eq!(edit_digest(Some(&change.after)), edit_digest(Some(&now)));
        assert_eq!(now["rotation"], json!(5.0));
        assert_eq!(now["crop"], straightened_crop(300.0, 200.0, 5.0));
        // Nothing else changed.
        let strip = |mut recipe: Value| {
            let map = recipe.as_object_mut().unwrap();
            for key in SOURCE_GEOMETRY_KEYS {
                map.remove(key);
            }
            map.remove("masks");
            recipe
        };
        // The drawn mask stays on the photo's content: it got the frame it
        // was drawn in.
        let parameters = &now["masks"][0]["subMasks"][0]["parameters"];
        assert!(!parameters["frame"].is_null(), "{parameters}");
        assert_eq!(parameters["centerX"], json!(100.0));
        assert_eq!(strip(now.clone()), strip(own.clone()));
        assert_eq!(
            (now["exposure"].as_f64(), now["contrast"].as_i64()),
            (Some(0.9), Some(12))
        );
        assert_eq!(fs::read(&file).unwrap(), original);

        // A repeated import no longer names it and keeps the edit.
        let again = import_into(&catalog, &level).unwrap();
        assert!(again.warnings.is_empty(), "{again:?}");
        assert!(again.geometry_offers.is_empty(), "{again:?}");
        assert!(
            again.recipe_changes.is_empty() && again.copies == 0,
            "{again:?}"
        );
        assert_eq!(
            catalog.editor_state(&asset.id).unwrap().unwrap()["adjustments"],
            now
        );
        // Reset in Capture One since the offer: the saved straightening is not
        // cleared by the action.
        let reset = source(&[("Exposure", "0.3"), ("Rotation", "0"), ("Crop", "0;0;0;0")]);
        let cleared = apply_source_geometry(&catalog, &reset, &offer);
        assert!(cleared.recipe_changes.is_empty(), "{cleared:?}");
        assert_eq!(cleared.errors.len(), 1, "{cleared:?}");
        assert_eq!(
            catalog.editor_state(&asset.id).unwrap().unwrap()["adjustments"],
            now
        );
        // Applying again changes nothing.
        let twice = apply_source_geometry(&catalog, &level, &offer);
        assert!(
            twice.recipe_changes.is_empty() && twice.errors.is_empty(),
            "{twice:?}"
        );
        assert_eq!(twice.unchanged, [path]);
        // A path that was not offered is left alone.
        let other = apply_source_geometry(
            &catalog,
            &level,
            &[GeometryOffer::of("elsewhere.NEF".into(), None)],
        );
        assert!(other.recipe_changes.is_empty() && other.unchanged.is_empty());
    }

    /// The geometry is read from the photo's frame: with the file offline the
    /// translation holds none, which must not clear the saved straightening.
    #[test]
    fn apply_source_geometry_writes_nothing_for_an_unreadable_photo() {
        let tmp = tempfile::tempdir().unwrap();
        let catalog = Library::create(&tmp.path().join("Rawtonic"), "Main", Mode::Catalog).unwrap();
        let file = tmp.path().join("a.png");
        image::RgbImage::new(300, 200).save(&file).unwrap();
        let entries = |pairs: &[(&str, &str)]| -> Vec<(String, String)> {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        };
        let source = |pairs: &[(&str, &str)]| Source {
            name: "C1".into(),
            photos: vec![photo(
                "1",
                file.clone(),
                Develop::CaptureOne(entries(pairs)),
                None,
            )],
            albums: vec![],
            stacks: vec![],
            keywords: Vec::new(),
            not_read: NotRead::default(),
        };
        import_into(&catalog, &source(&[("Exposure", "0.3")])).unwrap();
        let asset = catalog.asset_at(&file).unwrap().unwrap();
        let mut state = catalog.editor_state(&asset.id).unwrap().unwrap();
        state["adjustments"]["rotation"] = json!(2.0);
        state["adjustments"]["exposure"] = json!(0.9);
        catalog.save_editor_state(&asset.id, &state).unwrap();
        let own = catalog.editor_state(&asset.id).unwrap().unwrap()["adjustments"].clone();
        let level = source(&[("Exposure", "0.3"), ("Rotation", "5"), ("Crop", "0;0;0;0")]);
        let path = asset.path.to_string_lossy().into_owned();
        fs::remove_file(&file).unwrap();
        let offer = [GeometryOffer::of(
            path.clone(),
            Some(&json!({"rotation": 5.0})),
        )];
        let applied = apply_source_geometry(&catalog, &level, &offer);
        assert!(applied.recipe_changes.is_empty() && applied.unchanged.is_empty());
        assert_eq!(applied.errors.len(), 1, "{applied:?}");
        assert_eq!(
            catalog.editor_state(&asset.id).unwrap().unwrap()["adjustments"],
            own
        );
    }

    /// The report's field names the webview reads (`geometryOffers`) and the
    /// applied result's (`recipeChanges`, `unchanged`, `errors`).
    #[test]
    fn geometry_action_ipc_shapes() {
        let report = serde_json::to_value(ImportReport {
            geometry_offers: vec![GeometryOffer {
                path: "a.NEF".into(),
                rotation: 5.0,
                orientation_steps: 0.0,
            }],
            ..Default::default()
        })
        .unwrap();
        assert_eq!(
            report["geometryOffers"],
            json!([{"path": "a.NEF", "rotation": 5.0, "orientationSteps": 0.0}])
        );
        let applied = serde_json::to_value(GeometryApplied::default()).unwrap();
        for key in ["recipeChanges", "unchanged", "errors"] {
            assert!(applied[key].is_array(), "{key}");
        }
    }

    /// G50L review: the reader takes Capture One's rotation and crop since
    /// G50L. A session imported before and unchanged in Capture One since
    /// is no change: a repeated import keeps the photo's recipe (no "newer
    /// settings" copy beside an edit made here) and upgrades the record, so
    /// a later change in Capture One still comes in. Where Capture One
    /// straightens the photo, the earlier record cannot tell whether that
    /// was so then or done since: the edit stays, the import names it every
    /// time, the record is not upgraded.
    #[test]
    fn settings_read_since_an_earlier_import_are_no_change_there() {
        let entries = |pairs: &[(&str, &str)]| -> Vec<(String, String)> {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        };
        let session = |pairs: &[(&str, &str)]| {
            photo(
                "1",
                PathBuf::from("missing/a.NEF"),
                Develop::CaptureOne(entries(pairs)),
                None,
            )
        };
        let before = session(&[("Exposure", "0.3")]);
        let imported = json!({"exposure": 0.3});
        let edited = json!({"exposure": 0.9});
        let record = ImportRecord {
            source: source_digest(&before),
            translated: edit_digest(Some(&imported)),
            stored: edit_digest(Some(&imported)),
        };
        assert_eq!(earlier_source_digest(&before), None);

        // Unchanged in Capture One, nothing to straighten: kept, the record
        // upgraded; a later change there comes in.
        let level = session(&[("Exposure", "0.3"), ("Rotation", "0"), ("Crop", "0;0;0;0")]);
        assert_eq!(earlier_source_digest(&level), Some(record.source.clone()));
        for (own, rule) in [(&imported, Reimport::Same), (&edited, Reimport::Keep)] {
            let settled = settle_rule(
                Some(record.clone()),
                Some(own),
                None,
                &level,
                Some(&imported),
            );
            assert_eq!(settled.rule, rule);
            assert!(!settled.unread_straightening);
            assert_eq!(
                settled.upgrade.as_ref().unwrap().source,
                source_digest(&level)
            );
        }
        let upgraded = settle_rule(
            Some(record.clone()),
            Some(&imported),
            None,
            &level,
            Some(&imported),
        )
        .upgrade
        .unwrap();
        let changed = session(&[("Exposure", "0.7"), ("Rotation", "0"), ("Crop", "0;0;0;0")]);
        assert_eq!(
            settle_rule(
                Some(upgraded),
                Some(&imported),
                None,
                &changed,
                Some(&json!({"exposure": 0.7}))
            )
            .rule,
            Reimport::Apply
        );

        // Straightened in Capture One (then or since, the earlier record
        // cannot tell): the edit stays, named, the record kept as it was.
        let straightened = session(&[("Exposure", "0.3"), ("Rotation", "5"), ("Crop", "0;0;0;0")]);
        let incoming = json!({"exposure": 0.3, "rotation": 5.0});
        for own in [&imported, &edited] {
            let settled = settle_rule(
                Some(record.clone()),
                Some(own),
                None,
                &straightened,
                Some(&incoming),
            );
            assert_eq!(settled.rule, Reimport::Keep, "{own}");
            assert!(settled.unread_straightening);
            assert_eq!(settled.upgrade, None);
        }
        assert!(unread_straightening_line("a.NEF", Some(&incoming)).contains("straightened by 5°"));
        // Straightened here to match: settled, the record upgraded.
        let matched = json!({"exposure": 0.3, "rotation": 5.0});
        let settled = settle_rule(
            Some(record.clone()),
            Some(&matched),
            None,
            &straightened,
            Some(&incoming),
        );
        assert!(!settled.unread_straightening);
        assert!(settled.upgrade.is_some());
        // Matched by hand in the Crop tool (0.1° steps) to a fractional
        // angle: settled.
        let fractional = json!({"exposure": 0.3, "rotation": 5.13});
        let typed = json!({"exposure": 0.3, "rotation": 5.1});
        let settled = settle_rule(
            Some(record.clone()),
            Some(&typed),
            None,
            &straightened,
            Some(&fractional),
        );
        assert!(!settled.unread_straightening);
        assert!(settled.upgrade.is_some());
        // Straightened only here, Capture One's photo level: nothing to name,
        // the record upgraded.
        let local = json!({"exposure": 0.9, "rotation": 3.0});
        let settled = settle_rule(
            Some(record.clone()),
            Some(&local),
            None,
            &level,
            Some(&imported),
        );
        assert_eq!(settled.rule, Reimport::Keep);
        assert!(!settled.unread_straightening);
        assert!(settled.upgrade.is_some());
        // A record of today's reader is left alone.
        let today = ImportRecord {
            source: source_digest(&straightened),
            ..record.clone()
        };
        let settled = settle_rule(
            Some(today),
            Some(&imported),
            None,
            &straightened,
            Some(&incoming),
        );
        assert_eq!(settled.rule, Reimport::Keep);
        assert!(!settled.unread_straightening && settled.upgrade.is_none());
    }

    /// G50L: Capture One's "Generic pincushion distortion" profile is
    /// Rawtonic's polynomial distortion with k1 = +0.02 × amount at exact
    /// zoom 1 (filled to the corners, as Capture One crops), never a lens
    /// profile of the photo's lens at Capture One's amount.
    #[test]
    fn capture_one_generic_pincushion_profile_comes_over_as_the_polynomial() {
        let entries = |pairs: &[(&str, &str)]| -> Vec<(String, String)> {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        };
        let dir = tempfile::tempdir().unwrap();
        let raw = dir.path().join("a.NEF");
        fs::write(&raw, b"not a raw").unwrap();
        let pincushion = |distortion: &str, falloff: &str| {
            entries(&[
                ("LensIdGuid", "A4C7C116-2472-4B0F-9C39-5775CDAD2FF1"),
                ("LensGeoAbr", distortion),
                ("LensLightFallOff", falloff),
            ])
        };
        let c1 = pincushion("0,0;1000,20;2000,160|25.000000|1|100|-1", "|0|1");
        assert_eq!(capture_one_lens_amounts(&c1), None);
        let mut adjustments = json!({});
        capture_one_lens(&c1, &raw, &mut adjustments);
        assert_eq!(
            adjustments,
            json!({
                "lensCorrectionMode": "manual",
                "lensDistortionParams": {
                    "model": 0, "k1": 0.5, "k2": 0.0, "k3": 0.0,
                    "zoom": 1.0, "exact_zoom": true
                }
            })
        );
        assert!(capture_one_lens_report(&c1, &raw).is_empty());
        let mut full = json!({});
        capture_one_lens(&pincushion("|100.000000|1|100|-1", "|0|1"), &raw, &mut full);
        assert_eq!(full["lensDistortionParams"]["k1"], json!(2.0));
        // Its Light Falloff was not measured: reported, not carried.
        let with_falloff = pincushion("|50|1|100|-1", "|40|1");
        assert_eq!(
            capture_one_lens_report(&with_falloff, &raw),
            [
                "Capture One's Light Falloff 40 with the Generic pincushion profile not carried (no measured equivalent)"
            ]
        );
        // At 0 it corrects nothing in Capture One: the default stays, said once.
        let untouched = pincushion("|0.000000|1|100|-1", "|0|1");
        let mut kept = json!({});
        capture_one_lens(&untouched, &raw, &mut kept);
        assert_eq!(kept, json!({}));
        assert_eq!(capture_one_lens_report(&untouched, &raw).len(), 1);
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
        // G50S: its Distortion 30 is a pure radial cubic about the frame's
        // centre without any framing zoom, measured on Capture One's renders
        // as 0.0012 of the half diagonal per percent: Rawtonic's polynomial
        // model with k1 = −0.0012 × amount at zoom 1 and no lens profile.
        let mut adjustments = json!({});
        capture_one_lens(&generic, &raw, &mut adjustments);
        assert_eq!(
            adjustments,
            json!({
                "lensCorrectionMode": "manual",
                "lensDistortionParams": {
                    "model": 0, "k1": -0.036, "k2": 0.0, "k3": 0.0,
                    "zoom": 1.0, "exact_zoom": true
                }
            })
        );
        // Nothing to report: the Distortion came over (a Light Falloff of 0
        // is no setting).
        assert!(capture_one_lens_report(&generic, &raw).is_empty());
        // Light Falloff on the Generic profile has no equivalent: reported.
        let with_falloff = entries(&[
            ("LensIdGuid", CAPTURE_ONE_GENERIC_LENS),
            ("LensGeoAbr", "|30.0|1"),
            ("LensLightFallOff", "|20.0|1"),
        ]);
        assert_eq!(
            capture_one_lens_report(&with_falloff, &raw),
            [
                "Capture One's Light Falloff 20 with the Generic profile not carried (no measured equivalent)"
            ]
        );
        // Out of range values are clamped to the slider's 0…100.
        let mut clamped = json!({});
        capture_one_lens(
            &entries(&[
                ("LensIdGuid", CAPTURE_ONE_GENERIC_LENS),
                ("LensGeoAbr", "|250.0|1"),
            ]),
            &raw,
            &mut clamped,
        );
        assert_eq!(clamped["lensDistortionParams"]["k1"], json!(-0.12));
        let warnings = import_warnings(&photo(
            "1",
            raw.clone(),
            Develop::CaptureOne(generic.clone()),
            None,
        ));
        assert!(warnings.is_empty(), "{warnings:?}");
        // A Generic profile left at 0 is said once, with nothing left out.
        let untouched = entries(&[
            ("LensIdGuid", CAPTURE_ONE_GENERIC_LENS),
            ("LensGeoAbr", "0,0;1000,-10|0.000000|1|100|-1"),
            ("LensLightFallOff", "|0|1"),
        ]);
        assert_eq!(capture_one_lens_report(&untouched, &raw).len(), 1);
        let mut kept = json!({});
        capture_one_lens(&untouched, &raw, &mut kept);
        assert_eq!(
            kept,
            json!({}),
            "Distortion 0: the default lens correction stays"
        );
        // A lens profile of Capture One's own reports nothing.
        let profiled = entries(&[
            ("LensIdGuid", "P"),
            ("LensGeoAbr", "0,0;1000,-10|60.0|1|100|-1"),
            ("ChromaticAberEnabled", "1"),
        ]);
        assert!(capture_one_lens_report(&profiled, &raw).is_empty());

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
            capture_one_lens_report(&analysed, &raw),
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
            lens: None,
            copy_of: copy_of.map(str::to_string),
            copy_name: None,
            snapshots: Vec::new(),
            uid: None,
            capture_one_layers: Vec::new(),
        }
    }

    #[test]
    fn imported_capture_date_is_an_override_and_camera_facts_survive() {
        let dir = tempfile::tempdir().unwrap();
        let lib = Library::create(
            &dir.path().join("Library"),
            "Library",
            rawtonic_catalog::Mode::Catalog,
        )
        .unwrap();
        let path = dir.path().join("photo.NEF");
        std::fs::write(&path, b"synthetic raw").unwrap();
        let mut source = photo("1", path, Develop::None, None);
        source.metadata.capture = rawtonic_metadata::CaptureWrite::Set(
            rawtonic_metadata::XmpDateTime::parse("2016-09-11T18:16:08.7").unwrap(),
        );
        let (id, _) = import_photo(&lib, &source).unwrap();
        assert!(lib.capture_override(&id).unwrap().is_none());
        lib.record_facts(
            &id,
            &rawtonic_catalog::CaptureFacts {
                captured: Some("2016-09-10 18:16:08".into()),
                captured_subsec: Some("7".into()),
                captured_offset: Some("+02:00".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            lib.capture_override(&id).unwrap().unwrap().local,
            "2016-09-11 18:16:08"
        );
        assert_eq!(
            lib.capture_override(&id)
                .unwrap()
                .unwrap()
                .offset
                .as_deref(),
            Some("+02:00")
        );
        let current = lib.capture_override(&id).unwrap();
        lib.set_capture_times(&[rawtonic_catalog::capture_time::CaptureChange {
            asset_id: id.clone(),
            expected: current,
            new: None,
        }])
        .unwrap();
        assert!(lib.capture_override(&id).unwrap().is_none());
        assert_eq!(
            lib.facts(&id).unwrap().unwrap().0.captured.as_deref(),
            Some("2016-09-10 18:16:08")
        );
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
            keywords: Vec::new(),
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
            keywords: Vec::new(),
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
        // The photograph and its virtual copy, each a member by itself.
        assert_eq!(catalog.collection_assets(&italy.id).unwrap().len(), 1);
        assert_eq!(
            catalog
                .collection_item_paths(&italy.id)
                .unwrap()
                .iter()
                .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
                .collect::<Vec<_>>(),
            [
                "A.NEF".to_string(),
                format!(
                    "A.NEF?vc={}",
                    copy_id(
                        &catalog.asset_at(&photos.join("A.NEF")).unwrap().unwrap().id,
                        &copy_origin("LR", &source.photos[2]),
                        0
                    )
                )
            ]
        );
        assert_eq!(italy.count, 2);
        let best = collections.iter().find(|c| c.name == "Best picks").unwrap();
        // The photograph and its virtual copy (rated 4 and picked as well,
        // the copy's own pick) are both in it.
        assert_eq!((best.kind.as_str(), best.count), (SMART, 2));
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
        assert!(best.iter().any(|c| c.id == best_id && c.count == 2));
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
            keywords: Vec::new(),
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
                keywords: Vec::new(),
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
            keywords: Vec::new(),
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
                    {"field": "camera", "op": "words", "value": "Canon"},
                    {"field": "make", "op": "words", "value": "Canon"}]},
                {"combine": "any", "rules": [
                    {"field": "camera", "op": "words", "value": "EOS"},
                    {"field": "make", "op": "words", "value": "EOS"}]}]}),
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
                json!({"field": "lensImported", "op": "words", "value": "TAMRON"}),
            ),
            (
                "lens",
                "all",
                text("TAMRON"),
                json!({"field": "lensImported", "op": "contains", "value": "TAMRON"}),
            ),
            (
                "lens",
                "beginsWith",
                text("EF"),
                json!({"field": "lensImported", "op": "startsWith", "value": "EF"}),
            ),
            (
                "lens",
                "notEmpty",
                SmartValue::None,
                json!({"field": "lensImported", "op": "notEmpty"}),
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
        // Lightroom's whole name ("Canon EOS 6D") is the camera's one make
        // and model in the catalog.
        assert_eq!(
            one("camera", "==", text("Canon EOS 6D")).unwrap(),
            json!({"combine": "all", "rules": [
                {"field": "make", "op": "is", "value": "Canon"},
                {"field": "camera", "op": "is", "value": "EOS 6D"}]})
        );
        // Regional names stay cameras of their own, as in Lightroom.
        assert!(one("camera", "==", text("Canon EOS 600D")).is_some());
        // The start or end of the model, which the catalog also finds in
        // the names a camera's files state (`Canon EOS 6D` of `EOS 6D`).
        assert_eq!(
            one("camera", "beginsWith", text("Canon EOS")).unwrap(),
            json!({"field": "camera", "op": "startsWith", "value": "Canon EOS"})
        );
        assert_eq!(
            one("camera", "endsWith", text("D750")).unwrap(),
            json!({"field": "camera", "op": "endsWith", "value": "D750"})
        );
        // Copy status.
        assert_eq!(
            one("vcStatus", "==", text("virtualCopies")).unwrap(),
            json!({"field": "virtualCopy", "op": "is", "value": true})
        );
        assert_eq!(
            one("vcStatus", "==", text("masterImages")).unwrap(),
            json!({"field": "virtualCopy", "op": "is", "value": false})
        );
        assert_eq!(
            one("vcStatus", "!=", text("virtualCopies")).unwrap(),
            json!({"field": "virtualCopy", "op": "is", "value": false})
        );
        assert!(one("vcStatus", "==", text("other")).is_none());
        // A test Lightroom applies to each file's model that would select
        // other files here is left out.
        assert!(one("camera", "endsWith", text("1s")).is_none());
        assert!(one("camera", "beginsWith", text("EOS")).is_none());
        // One camera in the library for two models Lightroom tells apart.
        assert!(one("camera", "==", text("STYLUS1")).is_none());
        assert!(one("camera", "!=", text("STYLUS1,1s")).is_none());
        // A name only the decoder gives (headerless CHDK `… CRW` files) is
        // no second model.
        assert!(one("camera", "==", text("Canon PowerShot G7 X")).is_some());
        assert!(one("camera", "!=", text("Canon PowerShot S100")).is_some());
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
                .map(|(name, _, lens, _)| {
                    let mut p = photo(name, photos.join(name), Develop::None, None);
                    p.lens = (!lens.is_empty()).then(|| lens.to_string());
                    p
                })
                .collect(),
            albums: vec![],
            stacks: vec![],
            keywords: Vec::new(),
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
        // Lens rules match Lightroom's own lens names, kept by the import,
        // so neither the lens nor the file-name album waits for the files'
        // capture data.
        assert!(report.warnings.is_empty(), "{report:?}");
        assert!(report.smart_albums_by_facts.is_empty(), "{report:?}");
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
        assert_eq!(members("camera", "==", text("canon EOS 6D")), ["18b.CR2"]);
        assert_eq!(
            members("camera", "!=", text("Canon EOS 6D")),
            ["17a.CR2", "19c.DNG", "20d.NEF"]
        );
        // Lightroom's camera name is the files' model: `EOS 6D` is none.
        assert!(members("camera", "==", text("EOS 6D")).is_empty());
        assert_eq!(
            members("camera", "!=", text("EOS 6D")),
            ["17a.CR2", "18b.CR2", "19c.DNG", "20d.NEF"]
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

    /// The photographs (names, sorted) a Lightroom criterion, operation and
    /// its two values select.
    type Members = Box<dyn Fn(&str, &str, SmartValue, SmartValue) -> Vec<String>>;

    /// A catalog of photographs with the given capture facts and a closure
    /// that lists the photographs a Lightroom criterion selects.
    fn members_of_facts(
        facts: &[(&str, rawtonic_catalog::CaptureFacts)],
    ) -> (tempfile::TempDir, Members) {
        // Lightroom's lens name is the file's, unless a test gives another.
        let named: Vec<_> = facts
            .iter()
            .map(|(name, f)| (*name, f.clone(), f.lens.clone()))
            .collect();
        members_of_facts_named(&named)
    }

    /// `members_of_facts`, each photograph with the lens name its Lightroom
    /// catalog holds (which need not be the name the file states).
    fn members_of_facts_named(
        facts: &[(&str, rawtonic_catalog::CaptureFacts, Option<String>)],
    ) -> (tempfile::TempDir, Members) {
        let tmp = tempfile::tempdir().unwrap();
        let catalog = Library::create(&tmp.path().join("Rawtonic"), "Main", Mode::Catalog).unwrap();
        let photos = tmp.path().join("Photos");
        fs::create_dir_all(&photos).unwrap();
        let source = Source {
            name: "LR".into(),
            photos: facts
                .iter()
                .map(|(name, _, lens)| {
                    fs::write(photos.join(name), name).unwrap();
                    let mut p = photo(name, photos.join(name), Develop::None, None);
                    p.lens = lens.clone();
                    p
                })
                .collect(),
            albums: vec![],
            stacks: vec![],
            keywords: Vec::new(),
            not_read: NotRead::default(),
        };
        import_into(&catalog, &source).unwrap();
        for (name, facts, _) in facts {
            let id = catalog.asset_at(&photos.join(name)).unwrap().unwrap().id;
            catalog.record_facts(&id, facts).unwrap();
        }
        let members =
            move |criteria: &str, operation: &str, value: SmartValue, value2: SmartValue| {
                let rule = smart_rule(&SmartRules {
                    combine: "intersect".into(),
                    rules: vec![SmartRule::Condition {
                        criteria: criteria.into(),
                        operation: operation.into(),
                        value,
                        value2,
                        units: None,
                    }],
                })
                .unwrap_or_else(|| panic!("{criteria} {operation} does not translate"));
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
        (tmp, Box::new(members))
    }

    /// Lightroom Classic 15.6, measured on a 43-photograph catalog (SMR2):
    /// "words" for a camera or lens are whole words of the name (case
    /// ignored; brackets around a word do not count), "any" and "none of"
    /// find the text anywhere in it, "all" needs every word somewhere in it.
    #[test]
    fn lightroom_words_are_whole_words_of_the_camera_and_lens_names() {
        use rawtonic_catalog::CaptureFacts;
        let f = |model: &str, lens: &str| CaptureFacts {
            make: Some("Canon".into()),
            model: Some(model.into()),
            lens: (!lens.is_empty()).then(|| lens.into()),
            ..CaptureFacts::default()
        };
        let (_tmp, members) = members_of_facts(&[
            ("a.CR2", f("EOS 6D Mark II", "EF-S18-55mm f/3.5-5.6 IS")),
            (
                "b.CR2",
                f("EOS 600D", "TAMRON SP 90mm F/2.8 Di VC USD MACRO1:1 F017"),
            ),
            (
                "c.PEF",
                f("K-S2", "smc PENTAX-DA* 50-135mm F2.8 ED [IF] SDM"),
            ),
            ("d.DNG", f("M10", "Summicron-M 1:2/35 ASPH.")),
            ("e.CR2", f("EOS-1Ds Mark II", "85.0 mm")),
            ("f.NEF", f("D80", "")),
        ]);
        let text = |t: &str| SmartValue::Text(t.into());
        let lens = |op: &str, word: &str| members("lens", op, text(word), SmartValue::None);
        assert_eq!(lens("words", "EF-S18-55mm"), ["a.CR2"]);
        assert_eq!(lens("words", "ef-s18-55mm f/3.5-5.6"), ["a.CR2"]);
        assert_eq!(lens("words", "f/2.8"), ["b.CR2"]);
        assert_eq!(lens("words", "90mm TAMRON"), ["b.CR2"]);
        for part in [
            "EF",
            "18-55",
            "50mm",
            "TAM",
            "Summicron",
            "DA*",
            "[IF]",
            "1:2",
        ] {
            assert!(lens("words", part).is_empty(), "{part}");
        }
        assert_eq!(lens("words", "IF"), ["c.PEF"]);
        assert_eq!(lens("words", "PENTAX-DA*"), ["c.PEF"]);
        assert_eq!(lens("words", "Summicron-M"), ["d.DNG"]);
        assert_eq!(lens("words", "mm"), ["e.CR2"]);
        assert_eq!(lens("any", "tam"), ["b.CR2"]);
        assert_eq!(lens("any", "mm").len(), 5);
        // No lens at all is none of them.
        assert_eq!(
            lens("noneOf", "TAM"),
            ["a.CR2", "c.PEF", "d.DNG", "e.CR2", "f.NEF"]
        );
        assert_eq!(
            lens("noneOf", "18"),
            ["b.CR2", "c.PEF", "d.DNG", "e.CR2", "f.NEF"]
        );
        // The camera's words are whole words of its name as well.
        let camera = |op: &str, word: &str| members("camera", op, text(word), SmartValue::None);
        assert_eq!(camera("words", "Mark II"), ["a.CR2", "e.CR2"]);
        assert!(camera("words", "Mar").is_empty());
        assert_eq!(camera("words", "EOS-1Ds"), ["e.CR2"]);
        assert_eq!(camera("words", "EOS 6D"), ["a.CR2"]);
        assert_eq!(camera("any", "Mar"), ["a.CR2", "e.CR2"]);
        assert_eq!(camera("all", "Mar II"), ["a.CR2", "e.CR2"]);
    }

    /// Lightroom matches a lens rule against its own name for the lens, not
    /// the name the file states (SMR2, Lightroom Classic 15.6: its names come
    /// from its own tables, and from the focal range for a lens the file
    /// does not name). The names below are the ones its catalog held for
    /// these files.
    #[test]
    fn lightroom_lens_rules_match_the_lens_name_its_catalog_holds() {
        use rawtonic_catalog::CaptureFacts;
        let file = |lens: &str| CaptureFacts {
            make: Some("Canon".into()),
            lens: (!lens.is_empty()).then(|| lens.into()),
            ..CaptureFacts::default()
        };
        let (_tmp, members) = members_of_facts_named(&[
            // The file states a lens-database name, Lightroom its own.
            (
                "a.CR2",
                file("EF 50mm f/1.8 II"),
                Some("EF50mm f/1.8".into()),
            ),
            (
                "b.CR2",
                file("EF-S 18-55mm f/3.5-5.6 IS STM"),
                Some("EF-S18-55mm f/4-5.6 IS STM".into()),
            ),
            // The file states only the focal range, Lightroom a full name.
            (
                "c.CR2",
                file("17-50mm"),
                Some("TAMRON SP 17-50mm F/2.8 DiII A16E".into()),
            ),
            // A compact camera: no lens text in the file, a range in Lightroom.
            ("d.CR2", file(""), Some("6.1-30.5 mm".into())),
            // Lightroom records it without a lens, whatever the file states.
            ("e.CR2", file("EF 24mm f/2.8"), Some(String::new())),
            // Ingested after the import: only the file's lens text exists.
            ("f.CR2", file("EF 85mm f/1.8 USM"), None),
        ]);
        let text = |t: &str| SmartValue::Text(t.into());
        let lens = |op: &str, word: &str| members("lens", op, text(word), SmartValue::None);
        assert_eq!(lens("words", "EF50mm"), ["a.CR2"]);
        // Only the photograph Lightroom never saw has "EF" as a word.
        assert_eq!(lens("words", "EF"), ["f.CR2"]);
        assert_eq!(lens("words", "EF-S18-55mm"), ["b.CR2"]);
        assert_eq!(lens("words", "TAMRON 17-50mm"), ["c.CR2"]);
        assert_eq!(lens("words", "6.1-30.5"), ["d.CR2"]);
        assert_eq!(lens("==", "ef50mm f/1.8"), ["a.CR2"]);
        assert_eq!(lens("beginsWith", "EF"), ["a.CR2", "b.CR2", "f.CR2"]);
        assert_eq!(lens("endsWith", "STM"), ["b.CR2"]);
        assert_eq!(lens("any", "tam"), ["c.CR2"]);
        // A photograph Lightroom names no lens for is in "none of" and
        // "empty"; one it never saw is read by the file's lens.
        assert_eq!(lens("noneOf", "EF"), ["c.CR2", "d.CR2", "e.CR2"]);
        assert_eq!(
            members("lens", "empty", SmartValue::None, SmartValue::None),
            ["e.CR2"]
        );
        assert_eq!(lens("words", "85mm"), ["f.CR2"]);
        assert_eq!(lens("noneOf", "85mm").len(), 5);
    }

    /// A lens Lightroom now records as none replaces the name kept by an
    /// earlier import; an unreadable lens (no record) leaves it; an album
    /// edited here to other rules is never rewritten by a repeated import.
    #[test]
    fn a_repeated_import_updates_lens_names_and_keeps_edited_albums() {
        let tmp = tempfile::tempdir().unwrap();
        let catalog = Library::create(&tmp.path().join("Rawtonic"), "Main", Mode::Catalog).unwrap();
        let photos = tmp.path().join("Photos");
        fs::create_dir_all(&photos).unwrap();
        fs::write(photos.join("a.CR2"), "a").unwrap();
        let source = |lens: Option<&str>| {
            let mut p = photo("a", photos.join("a.CR2"), Develop::None, None);
            p.lens = lens.map(str::to_string);
            Source {
                name: "LR".into(),
                photos: vec![p],
                albums: vec![rawtonic_interop::Album {
                    key: "1".into(),
                    name: "Tamron".into(),
                    parent: None,
                    is_group: false,
                    photos: vec![],
                    smart: Some(SmartRules {
                        combine: "intersect".into(),
                        rules: vec![condition(
                            "lens",
                            "words",
                            SmartValue::Text("TAMRON".into()),
                        )],
                    }),
                }],
                stacks: vec![],
                keywords: Vec::new(),
                not_read: NotRead::default(),
            }
        };
        let tamron = |catalog: &Library| {
            let albums = catalog.collections().unwrap();
            let id = albums[0].id.clone();
            (albums.len(), catalog.collection_paths(&id).unwrap().len())
        };
        import_into(&catalog, &source(Some("TAMRON SP 17-50mm"))).unwrap();
        assert_eq!(tamron(&catalog), (1, 1));
        // The same album again: no second one.
        import_into(&catalog, &source(None)).unwrap();
        assert_eq!(tamron(&catalog), (1, 1), "nothing read: the name stays");
        // Lightroom records no lens any more.
        import_into(&catalog, &source(Some(""))).unwrap();
        assert_eq!(tamron(&catalog), (1, 0));
        // The photographer changes the album's rules: a repeated import
        // leaves it and adds Lightroom's beside it.
        let own = Rule::Group {
            combine: Combine::All,
            rules: vec![Rule::Condition {
                field: Field::Lens,
                op: Op::Words,
                value: json!("TAMRON"),
            }],
        };
        let id = catalog.collections().unwrap()[0].id.clone();
        catalog.set_collection_rules(&id, &own).unwrap();
        import_into(&catalog, &source(Some(""))).unwrap();
        let albums = catalog.collections().unwrap();
        assert_eq!(albums.len(), 2);
        assert_eq!(
            albums.iter().find(|a| a.id == id).unwrap().rules.as_ref(),
            Some(&own)
        );
    }

    /// A square bracket is dropped at the edge of a word of a name, not inside it.
    #[test]
    fn a_bracket_inside_a_word_of_a_name_is_part_of_it() {
        use rawtonic_catalog::CaptureFacts;
        let f = |lens: &str| CaptureFacts {
            lens: Some(lens.into()),
            ..CaptureFacts::default()
        };
        let (_tmp, members) =
            members_of_facts(&[("a.CR2", f("Zoom IMG[1] [IF]")), ("b.CR2", f("Zoom IMG1"))]);
        let lens = |word: &str| {
            members(
                "lens",
                "words",
                SmartValue::Text(word.into()),
                SmartValue::None,
            )
        };
        assert_eq!(lens("IMG[1]"), ["a.CR2"]);
        assert_eq!(lens("IMG1"), ["b.CR2"]);
        assert_eq!(lens("IF"), ["a.CR2"]);
        assert!(lens("[IF]").is_empty());
    }

    /// Lightroom Classic 15.6, measured (SMR2): aperture values are APEX
    /// (`2·log2(f-number)`, f/8 = 6), shutter speeds time values
    /// (`log2(1/seconds)`; greater is longer, a range runs from the shorter
    /// to the longer time), focal lengths millimetres, and equal means
    /// within 0.01 of that unit; ranges include both ends.
    #[test]
    fn lightroom_aperture_focal_length_and_shutter_criteria_select_as_lightroom_does() {
        use rawtonic_catalog::CaptureFacts;
        let f = |aperture: f64, focal: f64, exposure: f64| CaptureFacts {
            aperture: Some(aperture),
            focal_length: Some(focal),
            exposure: Some(exposure),
            ..CaptureFacts::default()
        };
        let (_tmp, members) = members_of_facts(&[
            ("f8.CR2", f(8.0, 50.0, 1.0 / 50.0)),
            ("f63.CR2", f(6.3, 15.067, 1.0 / 100.0)),
            ("f56.CR2", f(5.6, 15.0, 1.0 / 100.0)),
            ("f9.CR2", f(9.0, 8.898, 0.8)),
            ("f28.CR2", f(2.8, 200.0, 1.0 / 400.0)),
            // No aperture, focal length or shutter speed recorded.
            ("none.CR2", CaptureFacts::default()),
        ]);
        let n = |v: f64| SmartValue::Number(v);
        let one = |criteria: &str, op: &str, v: f64| members(criteria, op, n(v), SmartValue::None);
        let range = |criteria: &str, a: f64, b: f64| members(criteria, "in", n(a), n(b));
        // Aperture.
        assert_eq!(one("aperture", "==", 6.0), ["f8.CR2"]);
        assert_eq!(one("aperture", ">", 6.0), ["f9.CR2"]);
        assert_eq!(one("aperture", ">=", 6.0), ["f8.CR2", "f9.CR2"]);
        assert_eq!(one("aperture", "<", 6.0), ["f28.CR2", "f56.CR2", "f63.CR2"]);
        assert_eq!(one("aperture", "==", 5.32), ["f63.CR2"]);
        assert_eq!(one("aperture", "==", 5.3), Vec::<String>::new());
        assert_eq!(one("aperture", "==", 4.97), ["f56.CR2"]);
        assert_eq!(one("aperture", "==", 5.0), Vec::<String>::new());
        assert_eq!(
            range("aperture", 4.0, 6.0),
            ["f56.CR2", "f63.CR2", "f8.CR2"]
        );
        assert!(range("aperture", 6.0, 4.0).is_empty());
        assert_eq!(
            one("aperture", "!=", 6.0),
            ["f28.CR2", "f56.CR2", "f63.CR2", "f9.CR2"]
        );
        // Focal length.
        assert_eq!(one("focalLength", "==", 15.0), ["f56.CR2"]);
        assert_eq!(one("focalLength", "==", 15.07), ["f63.CR2"]);
        assert!(one("focalLength", "==", 15.1).is_empty());
        assert_eq!(one("focalLength", "==", 8.9), ["f9.CR2"]);
        assert_eq!(
            one("focalLength", ">", 15.0),
            ["f28.CR2", "f63.CR2", "f8.CR2"]
        );
        assert_eq!(one("focalLength", "<", 15.07), ["f56.CR2", "f9.CR2"]);
        assert_eq!(
            range("focalLength", 15.0, 50.0),
            ["f56.CR2", "f63.CR2", "f8.CR2"]
        );
        // Shutter speed.
        let tv = |seconds: f64| (1.0 / seconds).log2();
        assert_eq!(one("shutterSpeed", "==", 6.64), ["f56.CR2", "f63.CR2"]);
        assert_eq!(one("shutterSpeed", "==", tv(0.8)), ["f9.CR2"]);
        // Tv 6.6 (about 1/97 s) is outside Lightroom's ±0.01.
        assert!(one("shutterSpeed", "==", 6.6).is_empty());
        assert_eq!(one("shutterSpeed", "==", 6.65), ["f56.CR2", "f63.CR2"]);
        assert_eq!(
            one("shutterSpeed", "!=", 6.64),
            ["f28.CR2", "f8.CR2", "f9.CR2"]
        );
        assert_eq!(
            one("shutterSpeed", "<", 6.6),
            ["f28.CR2", "f56.CR2", "f63.CR2"]
        );
        assert_eq!(one("shutterSpeed", ">", 6.64), ["f8.CR2", "f9.CR2"]);
        assert_eq!(
            one("shutterSpeed", ">=", 6.64),
            ["f56.CR2", "f63.CR2", "f8.CR2", "f9.CR2"]
        );
        assert_eq!(one("shutterSpeed", "<", 6.64), ["f28.CR2"]);
        assert_eq!(
            one("shutterSpeed", "<=", 6.64),
            ["f28.CR2", "f56.CR2", "f63.CR2"]
        );
        assert_eq!(
            range("shutterSpeed", 6.64, 5.64),
            ["f56.CR2", "f63.CR2", "f8.CR2"]
        );
        assert!(range("shutterSpeed", 5.64, 6.64).is_empty());
        assert!(range("shutterSpeed", 6.64, 6.645).is_empty());
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
            keywords: Vec::new(),
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
    fn capture_one_import_reports_a_partially_translated_photo_color_editor() {
        let path = PathBuf::from("portrait.jpg");
        let develop =
            |entry: &str| Develop::CaptureOne(vec![("ColorCorrections".into(), entry.into())]);
        // This Skin Tone range translates, but its amount is approximate.
        let skin = "1,1,2,3,-10,1,180,120,90,-30,30,-100,100,15,0,0,0,0";
        let partial = photo("1", path.clone(), develop(skin), None);
        assert!(translate(&partial.develop).unwrap()["colorRanges"].is_array());
        assert_eq!(
            import_warnings(&partial),
            ["portrait.jpg: Capture One's Color Editor settings could not be fully translated"]
        );
        let advanced = skin.replacen("1,1,2,", "1,1,1,", 1);
        let full = photo("2", path, develop(&advanced), None);
        assert!(import_warnings(&full).is_empty());
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
            keywords: Vec::new(),
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
            keywords: Vec::new(),
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
            keywords: Vec::new(),
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

    /// G50S decisions 1 and 3 under the repeated-import rule (C1RI1): a
    /// Capture One import whose only translated setting is Generic
    /// Distortion never replaces an edit the user made here; it comes as a
    /// new virtual copy, said in the summary. An imported copy unchanged
    /// here follows it (undoable); a photo without an edit gets it.
    #[test]
    fn a_generic_distortion_import_never_replaces_the_users_edit() {
        let tmp = tempfile::tempdir().unwrap();
        let catalog = Library::create(&tmp.path().join("Cat"), "Cat", Mode::Catalog).unwrap();
        let raw = tmp.path().join("a.RAF");
        fs::write(&raw, b"raf").unwrap();
        let source = |generic: bool| Source {
            name: "s".into(),
            photos: vec![
                photo(
                    "1",
                    raw.clone(),
                    Develop::CaptureOne(if generic { generic_entries() } else { vec![] }),
                    None,
                ),
                photo(
                    "2",
                    raw.clone(),
                    Develop::CaptureOne(if generic { generic_entries() } else { vec![] }),
                    Some("1"),
                ),
            ],
            albums: vec![],
            stacks: vec![],
            keywords: Vec::new(),
            not_read: NotRead::default(),
        };
        fn generic_entries() -> Vec<(String, String)> {
            [
                ("LensIdGuid", CAPTURE_ONE_GENERIC_LENS),
                ("LensGeoAbr", "|40.0|1"),
            ]
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
        }
        // Imported before without Generic Distortion: the master carries a
        // user's edit afterwards, the copy exists as imported.
        import_into(&catalog, &source(false)).unwrap();
        let asset = catalog.asset_at(&raw).unwrap().unwrap().id;
        let copy = catalog.copies(&asset).unwrap()[0].clone();
        let edit = json!({"version": 1, "rating": 0, "tags": [],
            "adjustments": {"exposure": 0.7}});
        catalog.save_editor_state(&asset, &edit).unwrap();
        let report = import_into(&catalog, &source(true)).unwrap();
        let kept = catalog.editor_state(&asset).unwrap().unwrap();
        assert_eq!(kept["adjustments"]["exposure"], json!(0.7), "{report:?}");
        assert!(kept["adjustments"].get("lensDistortionParams").is_none());
        assert_eq!(
            report.edits_kept,
            [format!(
                "{}: kept your edit; the newer Capture One settings are in a new virtual copy",
                raw.display()
            )]
        );
        let copies = catalog.copies(&asset).unwrap();
        assert_eq!(copies.len(), 2, "{report:?}");
        for id in &copies {
            let state = catalog.copy_state(&asset, id).unwrap().unwrap();
            assert_eq!(
                state["adjustments"]["lensDistortionParams"]["k1"],
                json!(-0.048),
                "{id}"
            );
        }
        // The imported copy, unchanged here, followed as one undoable change.
        assert_eq!(report.recipe_changes.len(), 1, "{report:?}");
        assert!(
            report.recipe_changes[0]
                .path
                .ends_with(&format!("?vc={copy}"))
        );
        assert!(report.recipe_changes[0].before.is_null());
        // A photo without an edit of the user's gets the distortion.
        let fresh = tmp.path().join("b.RAF");
        fs::write(&fresh, b"raf").unwrap();
        let only = Source {
            name: "t".into(),
            photos: vec![photo(
                "1",
                fresh.clone(),
                Develop::CaptureOne(generic_entries()),
                None,
            )],
            albums: vec![],
            stacks: vec![],
            keywords: Vec::new(),
            not_read: NotRead::default(),
        };
        import_into(&catalog, &only).unwrap();
        let id = catalog.asset_at(&fresh).unwrap().unwrap().id;
        let state = catalog.editor_state(&id).unwrap().unwrap();
        assert_eq!(
            state["adjustments"]["lensDistortionParams"]["k1"],
            json!(-0.048)
        );
        // The user rescaled it since, Capture One unchanged: kept, silently.
        let mut state = catalog.editor_state(&id).unwrap().unwrap();
        state["adjustments"]["lensDistortionAmount"] = json!(50.0);
        catalog.save_editor_state(&id, &state).unwrap();
        let rescaled = import_into(&catalog, &only).unwrap();
        assert_eq!(
            catalog.editor_state(&id).unwrap().unwrap()["adjustments"]["lensDistortionAmount"],
            json!(50.0)
        );
        assert!(
            rescaled.edits_kept.is_empty() && rescaled.recipe_changes.is_empty(),
            "{rescaled:?}"
        );
        assert!(catalog.copies(&id).unwrap().is_empty());
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
            keywords: Vec::new(),
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

    /// The develop settings of an exposure in each other editor's form.
    fn exposure_in(editor: &str, ev: f64) -> Develop {
        match editor {
            "c1" => Develop::CaptureOne(vec![("Exposure".into(), format!("{ev}"))]),
            _ => Develop::LightroomXmp(format!(r#"crs:Exposure2012="{ev:+.2}""#)),
        }
    }

    fn one_photo(raw: &Path, develop: Develop) -> Source {
        Source {
            name: "Job".into(),
            photos: vec![photo("1", raw.to_path_buf(), develop, None)],
            albums: vec![],
            stacks: vec![],
            keywords: Vec::new(),
            not_read: NotRead::default(),
        }
    }

    fn exposure_of(catalog: &Library, asset: &str) -> Value {
        catalog.editor_state(asset).unwrap().unwrap()["adjustments"]["exposure"].clone()
    }

    /// C1RI1: an edit the photographer changed here since the import is
    /// never replaced by a repeated import, for Capture One and Lightroom
    /// alike. Settings changed there too come in as a new virtual copy,
    /// named in the summary, once; the photograph is never written.
    #[test]
    fn a_reimport_keeps_an_edit_changed_here_and_brings_the_new_settings_as_a_copy() {
        for editor in ["c1", "lr"] {
            let tmp = tempfile::tempdir().unwrap();
            let catalog = Library::create(&tmp.path().join("Cat"), "Cat", Mode::Catalog).unwrap();
            let raw = tmp.path().join("a.RAF");
            fs::write(&raw, b"raf").unwrap();
            import_into(&catalog, &one_photo(&raw, exposure_in(editor, 0.5))).unwrap();
            let asset = catalog.asset_at(&raw).unwrap().unwrap().id;
            assert_eq!(exposure_of(&catalog, &asset), json!(0.5), "{editor}");
            // The photographer's edit here.
            let mut state = catalog.editor_state(&asset).unwrap().unwrap();
            state["adjustments"]["exposure"] = json!(0.9);
            catalog.save_editor_state(&asset, &state).unwrap();
            // The other editor's settings changed as well.
            let changed = one_photo(&raw, exposure_in(editor, -0.3));
            let report = import_into(&catalog, &changed).unwrap();
            assert_eq!(
                exposure_of(&catalog, &asset),
                json!(0.9),
                "{editor} {report:?}"
            );
            assert!(report.recipe_changes.is_empty(), "{editor} {report:?}");
            let copies = catalog.copies(&asset).unwrap();
            assert_eq!(copies.len(), 1, "{editor} {report:?}");
            let copy = catalog.copy_state(&asset, &copies[0]).unwrap().unwrap();
            assert_eq!(copy["adjustments"]["exposure"], json!(-0.3), "{editor}");
            let name = if editor == "c1" {
                "Capture One"
            } else {
                "Lightroom"
            };
            assert_eq!(
                report.edits_kept,
                [format!(
                    "{}: kept your edit; the newer {name} settings are in a new virtual copy",
                    raw.display()
                )],
                "{editor}"
            );
            assert_eq!(report.copies, 1, "{editor}");
            // Its derived sidecar beside the photograph, as for every copy.
            assert!(
                raw.with_file_name(format!("a.RAF.{}.rrdata", copies[0]))
                    .is_file()
            );
            // Repeated: neither a second copy nor a second line.
            let again = import_into(&catalog, &changed).unwrap();
            assert_eq!(catalog.copies(&asset).unwrap(), copies, "{editor}");
            assert!(again.edits_kept.is_empty(), "{editor} {again:?}");
            assert_eq!(exposure_of(&catalog, &asset), json!(0.9));
            assert_eq!(fs::read(&raw).unwrap(), b"raf");
        }
    }

    /// C1RI1: an edit still exactly as the last import left it follows the
    /// other editor's changed settings, as one undoable change, and the
    /// derived sidecar beside the photograph follows; unchanged settings
    /// there leave an edit made here alone, without a line.
    #[test]
    fn a_reimport_updates_an_edit_unchanged_here_with_undo() {
        for editor in ["c1", "lr"] {
            let tmp = tempfile::tempdir().unwrap();
            let catalog = Library::create(&tmp.path().join("Cat"), "Cat", Mode::Catalog).unwrap();
            let raw = tmp.path().join("a.RAF");
            fs::write(&raw, b"raf").unwrap();
            let first = one_photo(&raw, exposure_in(editor, 0.5));
            import_into(&catalog, &first).unwrap();
            let asset = catalog.asset_at(&raw).unwrap().unwrap();
            let report =
                import_into(&catalog, &one_photo(&raw, exposure_in(editor, -0.3))).unwrap();
            assert_eq!(
                exposure_of(&catalog, &asset.id),
                json!(-0.3),
                "{editor} {report:?}"
            );
            assert!(catalog.copies(&asset.id).unwrap().is_empty(), "{editor}");
            assert!(report.edits_kept.is_empty(), "{editor} {report:?}");
            assert_eq!(report.recipe_changes.len(), 1, "{editor} {report:?}");
            let change = &report.recipe_changes[0];
            assert_eq!(change.path, asset.path.to_string_lossy(), "{editor}");
            assert_eq!(change.before["exposure"], json!(0.5), "{editor}");
            assert_eq!(change.after["exposure"], json!(-0.3), "{editor}");
            let sidecar: Value =
                serde_json::from_slice(&fs::read(raw.with_file_name("a.RAF.rrdata")).unwrap())
                    .unwrap();
            assert_eq!(sidecar["adjustments"]["exposure"], json!(-0.3), "{editor}");
            // An edit here, settings there unchanged: kept, nothing to say.
            let mut state = catalog.editor_state(&asset.id).unwrap().unwrap();
            state["adjustments"]["exposure"] = json!(0.9);
            catalog.save_editor_state(&asset.id, &state).unwrap();
            let same = import_into(&catalog, &one_photo(&raw, exposure_in(editor, -0.3))).unwrap();
            assert_eq!(
                exposure_of(&catalog, &asset.id),
                json!(0.9),
                "{editor} {same:?}"
            );
            assert!(
                same.recipe_changes.is_empty() && same.edits_kept.is_empty(),
                "{same:?}"
            );
            assert!(catalog.copies(&asset.id).unwrap().is_empty(), "{editor}");
            assert_eq!(fs::read(&raw).unwrap(), b"raf");
        }
    }

    /// C1RI1 for virtual copies: an imported copy unchanged here follows
    /// the other editor's variant (undoable, its sidecar follows); one
    /// edited here is kept and the variant's new settings come as a further
    /// copy.
    #[test]
    fn a_reimport_treats_imported_copies_as_edits() {
        let tmp = tempfile::tempdir().unwrap();
        let catalog = Library::create(&tmp.path().join("Cat"), "Cat", Mode::Catalog).unwrap();
        let raw = tmp.path().join("a.RAF");
        fs::write(&raw, b"raf").unwrap();
        let source = |copy_ev: f64| Source {
            name: "Job".into(),
            photos: vec![
                photo("1", raw.clone(), exposure_in("c1", 0.5), None),
                photo("2", raw.clone(), exposure_in("c1", copy_ev), Some("1")),
            ],
            albums: vec![],
            stacks: vec![],
            keywords: Vec::new(),
            not_read: NotRead::default(),
        };
        import_into(&catalog, &source(1.0)).unwrap();
        let asset = catalog.asset_at(&raw).unwrap().unwrap().id;
        let copies = catalog.copies(&asset).unwrap();
        assert_eq!(copies.len(), 1);
        let copy = copies[0].clone();
        let report = import_into(&catalog, &source(1.5)).unwrap();
        let state = catalog.copy_state(&asset, &copy).unwrap().unwrap();
        assert_eq!(state["adjustments"]["exposure"], json!(1.5), "{report:?}");
        assert_eq!(state["rating"], json!(4));
        let change = report
            .recipe_changes
            .iter()
            .find(|c| c.path.ends_with(&format!("?vc={copy}")))
            .unwrap_or_else(|| panic!("{report:?}"));
        assert_eq!(change.before["exposure"], json!(1.0));
        let sidecar: Value = serde_json::from_slice(
            &fs::read(raw.with_file_name(format!("a.RAF.{copy}.rrdata"))).unwrap(),
        )
        .unwrap();
        assert_eq!(sidecar["adjustments"]["exposure"], json!(1.5));
        // Edited here, changed there: kept, a further copy.
        let mut state = state;
        state["adjustments"]["exposure"] = json!(2.0);
        catalog.save_copy_state(&asset, &copy, &state).unwrap();
        let report = import_into(&catalog, &source(-1.0)).unwrap();
        let kept = catalog.copy_state(&asset, &copy).unwrap().unwrap();
        assert_eq!(kept["adjustments"]["exposure"], json!(2.0), "{report:?}");
        let now = catalog.copies(&asset).unwrap();
        assert_eq!(now.len(), 2, "{report:?}");
        let added = now.iter().find(|c| **c != copy).unwrap();
        let added = catalog.copy_state(&asset, added).unwrap().unwrap();
        assert_eq!(added["adjustments"]["exposure"], json!(-1.0));
        assert_eq!(report.edits_kept.len(), 1, "{report:?}");
        assert_eq!(fs::read(&raw).unwrap(), b"raf");
    }

    /// C1RI1 review: settings unchanged there write nothing even when their
    /// translation differs (new mask ids, a newer converter); nothing to
    /// translate there never clears an edit; an edit another catalog saved
    /// in the sidecar beside the photo counts as the photographer's: never
    /// replaced, never in the Undo step.
    #[test]
    fn a_reimport_leaves_unchanged_sources_defaults_and_foreign_sidecars_alone() {
        // The rule alone: the same settings there, a translation that
        // differs only in generated ids or by a newer converter.
        let with_id = |id: &str, ev: f64| json!({"exposure": ev, "masks": [{"id": id, "subMasks": [{"id": id}]}]});
        let record = ImportRecord {
            source: "s1".into(),
            translated: edit_digest(Some(&with_id("a", 0.5))),
            stored: edit_digest(Some(&with_id("a", 0.5))),
        };
        let own = with_id("a", 0.5);
        assert_eq!(
            reimport(Some(&record), Some(&own), "s1", Some(&with_id("b", 0.5)),),
            Reimport::Same
        );
        assert_eq!(
            reimport(Some(&record), Some(&own), "s1", Some(&with_id("b", 0.6)),),
            Reimport::Keep
        );
        assert_eq!(
            reimport(Some(&record), Some(&own), "s2", Some(&with_id("b", 0.6)),),
            Reimport::Apply
        );
        // Nothing to translate there never clears an edit (a reset cannot
        // be told from settings the reader could not read).
        assert_eq!(
            reimport(Some(&record), Some(&own), "s2", None),
            Reimport::Keep
        );

        let tmp = tempfile::tempdir().unwrap();
        let catalog = Library::create(&tmp.path().join("Cat"), "Cat", Mode::Catalog).unwrap();
        let raw = tmp.path().join("a.RAF");
        fs::write(&raw, b"raf").unwrap();
        import_into(&catalog, &one_photo(&raw, exposure_in("c1", 0.5))).unwrap();
        let asset = catalog.asset_at(&raw).unwrap().unwrap().id;
        let reset = import_into(&catalog, &one_photo(&raw, Develop::CaptureOne(vec![]))).unwrap();
        assert_eq!(exposure_of(&catalog, &asset), json!(0.5), "{reset:?}");
        assert!(reset.recipe_changes.is_empty(), "{reset:?}");

        // A copy whose sidecar another catalog over the same folder changed:
        // the library follows the import, the sidecar stays, and it is said.
        let source = |copy_ev: f64| Source {
            name: "Job".into(),
            photos: vec![
                photo("1", raw.clone(), Develop::CaptureOne(vec![]), None),
                photo("2", raw.clone(), exposure_in("c1", copy_ev), Some("1")),
            ],
            albums: vec![],
            stacks: vec![],
            keywords: Vec::new(),
            not_read: NotRead::default(),
        };
        import_into(&catalog, &source(1.0)).unwrap();
        let copy = catalog.copies(&asset).unwrap()[0].clone();
        let sidecar = raw.with_file_name(format!("a.RAF.{copy}.rrdata"));
        let mut foreign: Value = serde_json::from_slice(&fs::read(&sidecar).unwrap()).unwrap();
        foreign["adjustments"]["exposure"] = json!(3.0);
        let foreign = serde_json::to_vec_pretty(&foreign).unwrap();
        fs::write(&sidecar, &foreign).unwrap();
        let report = import_into(&catalog, &source(1.5)).unwrap();
        assert_eq!(
            catalog.copy_state(&asset, &copy).unwrap().unwrap()["adjustments"]["exposure"],
            json!(1.0),
            "{report:?}"
        );
        assert_eq!(fs::read(&sidecar).unwrap(), foreign);
        assert!(report.recipe_changes.is_empty(), "{report:?}");
        assert_eq!(report.edits_kept.len(), 1, "{report:?}");
        assert_eq!(catalog.copies(&asset).unwrap().len(), 2);
        assert_eq!(fs::read(&raw).unwrap(), b"raf");
    }

    /// C1RI1 review round 3 (finding 4): an imported copy whose sidecar
    /// beside the photo cannot be written (a read-only originals folder) is
    /// not updated: the library keeps the copy as the editor reads it, it is
    /// no part of the Undo step, and the next import updates it.
    #[cfg(unix)]
    #[test]
    fn a_copy_whose_sidecar_cannot_be_written_is_updated_by_the_next_import() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let catalog = Library::create(&tmp.path().join("Cat"), "Cat", Mode::Catalog).unwrap();
        let photos = tmp.path().join("Photos");
        fs::create_dir_all(&photos).unwrap();
        let raw = photos.join("a.RAF");
        fs::write(&raw, b"raf").unwrap();
        let source = |copy_ev: f64| Source {
            name: "Job".into(),
            photos: vec![
                photo("1", raw.clone(), exposure_in("c1", 0.5), None),
                photo("2", raw.clone(), exposure_in("c1", copy_ev), Some("1")),
            ],
            albums: vec![],
            stacks: vec![],
            keywords: Vec::new(),
            not_read: NotRead::default(),
        };
        import_into(&catalog, &source(1.0)).unwrap();
        let asset = catalog.asset_at(&raw).unwrap().unwrap().id;
        let copy = catalog.copies(&asset).unwrap()[0].clone();
        let sidecar = raw.with_file_name(format!("a.RAF.{copy}.rrdata"));
        let written = fs::read(&sidecar).unwrap();
        fs::set_permissions(&photos, fs::Permissions::from_mode(0o555)).unwrap();
        let report = import_into(&catalog, &source(1.5));
        fs::set_permissions(&photos, fs::Permissions::from_mode(0o755)).unwrap();
        let report = report.unwrap();
        assert_eq!(
            catalog.copy_state(&asset, &copy).unwrap().unwrap()["adjustments"]["exposure"],
            json!(1.0),
            "{report:?}"
        );
        assert!(
            report
                .recipe_changes
                .iter()
                .all(|c| !c.path.contains(&copy)),
            "{report:?}"
        );
        assert!(!report.errors.is_empty(), "{report:?}");
        assert_eq!(fs::read(&sidecar).unwrap(), written);
        // Writable again: the same settings update it, and its file follows.
        let again = import_into(&catalog, &source(1.5)).unwrap();
        assert_eq!(
            catalog.copy_state(&asset, &copy).unwrap().unwrap()["adjustments"]["exposure"],
            json!(1.5),
            "{again:?}"
        );
        let followed: Value = serde_json::from_slice(&fs::read(&sidecar).unwrap()).unwrap();
        assert_eq!(followed["adjustments"]["exposure"], json!(1.5));
        assert_eq!(fs::read(&raw).unwrap(), b"raf");
    }

    /// C1RI1 review round 3 (finding 3): a Capture One layer whose painted
    /// coverage cannot be read (an incomplete copy of the session) never
    /// makes a repeated import replace the edit holding that layer.
    #[test]
    fn unreadable_layer_coverage_never_replaces_an_edit() {
        use rawtonic_interop::capture_one::Layer;
        use rawtonic_interop::capture_one_masks::{Mask, Raster};
        let tmp = tempfile::tempdir().unwrap();
        let catalog = Library::create(&tmp.path().join("Cat"), "Cat", Mode::Catalog).unwrap();
        let raw = tmp.path().join("a.RAF");
        fs::write(&raw, b"raf").unwrap();
        let layered = |raster: Raster| {
            let mut source = one_photo(&raw, exposure_in("c1", 0.5));
            source.photos[0].capture_one_layers = vec![Layer {
                name: "Sky".into(),
                enabled: true,
                opacity: 100.0,
                settings: vec![("Exposure".into(), "-1".into())],
                mask: Mask::Raster { raster, ai: false },
                frame: (128.0, 128.0),
            }];
            source
        };
        import_into(
            &catalog,
            &layered(Raster::from_values(128, 128, &[200; 128 * 128])),
        )
        .unwrap();
        let asset = catalog.asset_at(&raw).unwrap().unwrap().id;
        let before = catalog.editor_state(&asset).unwrap().unwrap()["adjustments"].clone();
        let report = import_into(&catalog, &layered(Raster::unreadable(128, 128))).unwrap();
        assert_eq!(
            catalog.editor_state(&asset).unwrap().unwrap()["adjustments"],
            before,
            "{report:?}"
        );
        assert!(report.recipe_changes.is_empty(), "{report:?}");
        assert!(catalog.copies(&asset).unwrap().is_empty(), "{report:?}");
        // The reader's other outcomes for a damaged or incomplete session:
        // a mask record it did not understand, one it did not find.
        for mask in [
            Mask::Unsupported {
                what: "damaged".into(),
            },
            Mask::Missing,
        ] {
            let mut source = layered(Raster::from_values(128, 128, &[200; 128 * 128]));
            source.photos[0].capture_one_layers[0].mask = mask.clone();
            let report = import_into(&catalog, &source).unwrap();
            assert_eq!(
                catalog.editor_state(&asset).unwrap().unwrap()["adjustments"],
                before,
                "{mask:?} {report:?}"
            );
            assert!(report.recipe_changes.is_empty(), "{mask:?} {report:?}");
        }
    }
}
