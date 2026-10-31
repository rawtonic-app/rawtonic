//! Library search: one rule model for the Library's filter and for smart
//! albums (G36/G37). A rule is a condition on one field of a photograph
//! (text, keywords, rating, flag, label, file type, camera, lens, exposure,
//! capture or import date, album, folder, offline, AI edits, imported
//! provenance) or a group of rules that
//! must all, any or none hold, nested as deep as needed: Lightroom's smart
//! collection rules and Capture One's smart album and filter criteria by
//! their documented capability, as one tool. Rules are compiled to one SQL
//! query over the catalog (no file is read), so a smart album is always
//! current: it is evaluated whenever it is shown or counted.
use crate::Library;
use anyhow::{Context, Result, bail, ensure};
use rawtonic_metadata::provenance::{AI_FACTS, RAW_ONLY};
use rusqlite::{
    ToSql,
    types::{Value as Sql, ValueRef},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::{Path, PathBuf},
};

/// How a group combines its rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Combine {
    /// Every rule holds (Lightroom "all", Capture One "Match all").
    #[default]
    All,
    /// At least one rule holds.
    Any,
    /// No rule holds.
    None,
}

/// A rule: a group of rules or one condition.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Rule {
    Group {
        combine: Combine,
        rules: Vec<Rule>,
    },
    Condition {
        field: Field,
        op: Op,
        #[serde(default, skip_serializing_if = "Value::is_null")]
        value: Value,
    },
}

/// What a condition looks at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
pub enum Field {
    /// Any searchable text: path, keywords, title, caption, creator, IPTC
    /// fields, camera make and model, lens.
    Text,
    /// The same text without the keywords: what a photograph's virtual
    /// copies share with it (each copy keeps keywords of its own).
    Details,
    /// The file's name.
    Filename,
    Keyword,
    /// Camera model (as Lightroom shows it: `ILCE-7RM4`, `Canon EOS R5`).
    Camera,
    /// Camera maker (`SONY`, `Canon`).
    Make,
    Lens,
    /// File type category (`raw`, `dng`, `jpeg`, `heif`, `tiff`, `png`,
    /// `other`).
    FileType,
    Rating,
    /// `picked`, `rejected`, `unflagged`.
    Flag,
    /// Colour label (`red`, `yellow`, `green`, `blue`, `purple`; `none`).
    Label,
    Iso,
    /// Millimetres.
    FocalLength,
    /// F-number.
    Aperture,
    /// Exposure time in seconds.
    Exposure,
    CaptureDate,
    /// When the photograph came into the library.
    ImportDate,
    /// Album, album group or smart album (by id).
    Album,
    /// A folder (absolute path), with or without its subfolders.
    Folder,
    /// The file is missing or on a disconnected drive.
    Offline,
    /// In the newest import batch (Lightroom's Previous Import).
    PreviousImport,
    /// What the recipe does with AI (`rawtonic_metadata::provenance::
    /// AI_FACTS` by name: generated areas and the model that filled them,
    /// AI selections, AI Denoise, AI results still missing; `none` for
    /// none). A photograph has the facts of its own recipe and its virtual
    /// copies' (copies are found as their photograph).
    Ai,
    /// The IPTC Digital Source Type the file states (its IPTC code,
    /// `trainedAlgorithmicMedia`; `none` for none).
    SourceType,
    /// Another present photograph holds identical image data (Find
    /// Duplicates' hashes as recorded, `duplicates.rs`; photographs not
    /// hashed yet are not duplicates).
    Duplicate,
}

/// How a condition compares.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Op {
    /// Text: every word is part of the field. Album: in it. Folder: in it
    /// or a subfolder.
    Contains,
    /// Text: at least one word is part of the field.
    ContainsAny,
    /// Text: no word is part of the field. Album/folder: not in it.
    NotContains,
    /// Equal (text ignoring case; a keyword of the photograph; one of a
    /// list for flag, label, file type; a folder without its subfolders;
    /// a calendar day for dates; `true`/`false` for offline).
    Is,
    IsNot,
    StartsWith,
    EndsWith,
    /// No value (no keyword, no camera, an unrated photograph, …).
    Empty,
    NotEmpty,
    Lt,
    Le,
    Gt,
    Ge,
    /// Inclusive range `[from, to]` (numbers or days).
    Between,
    /// Dates: `{ "amount": n, "unit": "days" | "weeks" | "months" | "years" }`
    /// up to today (Lightroom "is in the last").
    InLast,
    NotInLast,
}

/// Nesting deeper than this, or more conditions than `MAX_RULES`, is
/// refused (a hand-edited or damaged rule set must not build a query
/// SQLite rejects).
const MAX_DEPTH: usize = 16;
const MAX_RULES: usize = 500;
/// Albums, groups and smart albums one rule may expand in all (smart
/// albums naming groups and each other).
const MAX_EXPANSIONS: usize = 256;

/// File extensions of each file type category; any other extension is a
/// RAW (the library only registers photographs; the app's format lists are
/// checked against this in `src-tauri/src/library_search.rs`).
const FILE_TYPES: &[(&str, &[&str])] = &[
    ("dng", &["dng"]),
    ("jpeg", &["jpg", "jpeg", "jpe"]),
    ("heif", &["heic", "heif", "hif", "avif"]),
    ("tiff", &["tif", "tiff"]),
    ("png", &["png"]),
    (
        "other",
        &[
            "webp", "jxl", "psd", "bmp", "gif", "exr", "hdr", "tga", "ico", "dds", "qoi", "ff",
            "pnm", "pbm", "pgm", "ppm", "pam",
        ],
    ),
];

/// The file type category of an extension (lower case).
pub fn file_type(extension: &str) -> &'static str {
    FILE_TYPES
        .iter()
        .find(|(_, extensions)| extensions.contains(&extension))
        .map_or("raw", |(kind, _)| kind)
}

const LABELS: &[&str] = &["red", "yellow", "green", "blue", "purple"];

impl Rule {
    /// Every word of `text` in any searchable text (the search box).
    pub fn text(text: &str) -> Self {
        Self::Condition {
            field: Field::Text,
            op: Op::Contains,
            value: Value::String(text.to_string()),
        }
    }

    /// The photographs of an album, group or smart album.
    pub fn album(id: &str) -> Self {
        Self::Condition {
            field: Field::Album,
            op: Op::Is,
            value: Value::String(id.to_string()),
        }
    }

    /// The rule as a library keeps it: folders inside `directory` relative
    /// to it, so a session moved while closed keeps its smart albums.
    pub fn relative_to(&self, directory: &Path) -> Result<Self> {
        Ok(match self {
            Self::Group { combine, rules } => Self::Group {
                combine: *combine,
                rules: rules
                    .iter()
                    .map(|r| r.relative_to(directory))
                    .collect::<Result<_>>()?,
            },
            Self::Condition {
                field: Field::Folder,
                op,
                value: Value::String(folder),
            } => {
                let value = match Path::new(folder).strip_prefix(directory) {
                    // The library's own folder is ".".
                    Ok(relative)
                        if Path::new(folder).is_absolute() && relative.as_os_str().is_empty() =>
                    {
                        ".".to_string()
                    }
                    Ok(relative) if Path::new(folder).is_absolute() => {
                        // A rule is read back with `directory.join`, so its
                        // relative value is the stored `/` form (`Day 2`,
                        // never a native `Day 2\` separator). A folder this
                        // library cannot name is rejected before saving.
                        crate::portable_relative(relative)?
                    }
                    _ => folder.clone(),
                };
                Self::Condition {
                    field: Field::Folder,
                    op: *op,
                    value: Value::String(value),
                }
            }
            other => other.clone(),
        })
    }

    /// Every photograph.
    pub fn everything() -> Self {
        Self::Group {
            combine: Combine::All,
            rules: Vec::new(),
        }
    }

    /// Refuses rules that cannot be evaluated (unknown values, wrong
    /// operators for a field, too deep), with what is wrong.
    pub fn validate(&self) -> Result<()> {
        let mut count = 0;
        self.check(0, &mut count)
    }

    fn check(&self, depth: usize, count: &mut usize) -> Result<()> {
        ensure!(depth <= MAX_DEPTH, "Rules are nested too deeply");
        match self {
            Self::Group { rules, .. } => rules.iter().try_for_each(|r| r.check(depth + 1, count)),
            Self::Condition { field, op, value } => {
                *count += 1;
                ensure!(*count <= MAX_RULES, "Too many rules");
                // Compiling checks operator and value; the album a rule
                // names may be gone (then it matches nothing).
                let mut sql = Compiler::new(Path::new("."), None);
                sql.condition(*field, *op, value).map(|_| ())
            }
        }
    }
}

/// Builds a WHERE clause over `assets a JOIN metadata m JOIN roots r`.
struct Compiler<'a> {
    directory: &'a Path,
    library: Option<&'a Library>,
    params: Vec<Sql>,
    /// Smart albums being expanded (one that refers to itself, directly or
    /// through others, matches nothing there).
    expanding: HashSet<String>,
    /// Groups and smart albums expanded so far (`expand`).
    expansions: usize,
    today: chrono::NaiveDate,
    /// The condition reads `metadata m` / `roots r`.
    metadata: bool,
    roots: bool,
    /// The condition compares image data (`Field::Duplicate`).
    duplicates: bool,
    /// The separator of the stored paths (`rel_path`, `roots.path`): the
    /// platform's, as `Path` gives them when photographs are registered.
    sep: char,
}

fn words(value: &Value) -> Result<Vec<String>> {
    let text = value.as_str().context("Text rules need a text value")?;
    ensure!(text.len() <= 4096, "Search text is too long");
    Ok(text.split_whitespace().map(str::to_string).collect())
}

fn text_value(value: &Value) -> Result<String> {
    let text = value.as_str().context("This rule needs a text value")?;
    ensure!(text.len() <= 4096, "Rule text is too long");
    Ok(text.trim().to_string())
}

fn number(value: &Value) -> Result<f64> {
    value
        .as_f64()
        .filter(|n| n.is_finite())
        .context("This rule needs a number")
}

fn pair<T>(value: &Value, read: impl Fn(&Value) -> Result<T>) -> Result<(T, T)> {
    match value.as_array().map(Vec::as_slice) {
        Some([from, to]) => Ok((read(from)?, read(to)?)),
        _ => bail!("A range needs two values"),
    }
}

/// A list of names (`"red"` or `["red","blue"]`), lower case.
fn names(value: &Value) -> Result<Vec<String>> {
    let list = match value {
        Value::String(one) => vec![one.as_str()],
        Value::Array(many) => many
            .iter()
            .map(|v| v.as_str().context("Expected names"))
            .collect::<Result<_>>()?,
        _ => bail!("Expected a name or a list of names"),
    };
    ensure!(!list.is_empty(), "Expected at least one name");
    Ok(list.into_iter().map(str::to_lowercase).collect())
}

/// `names` as given (IPTC codes are camel case).
fn names_verbatim(value: &Value) -> Result<Vec<String>> {
    let list = match value {
        Value::String(one) => vec![one.as_str()],
        Value::Array(many) => many
            .iter()
            .map(|v| v.as_str().context("Expected names"))
            .collect::<Result<_>>()?,
        _ => bail!("Expected a name or a list of names"),
    };
    ensure!(!list.is_empty(), "Expected at least one name");
    Ok(list
        .into_iter()
        .map(|name| name.trim().to_string())
        .collect())
}

/// The bit of an AI fact by its rule name (`AI_FACTS`), any case.
fn ai_fact(name: &str) -> Result<u32> {
    AI_FACTS
        .iter()
        .find(|fact| fact.name.eq_ignore_ascii_case(name))
        .map(|fact| fact.bit)
        .with_context(|| format!("Unknown AI edit {name}"))
}

/// Every AI fact.
const AI_ANY: u32 = {
    let mut bits = 0;
    let mut i = 0;
    while i < AI_FACTS.len() {
        bits |= AI_FACTS[i].bit;
        i += 1;
    }
    bits
};

/// The photographs one of whose versions (its recipe, a virtual copy) has
/// any of `bits`. A constant in the text: it selects the partial index of
/// versions with facts (`versions_ai_facts`).
fn ai_test(bits: u32) -> String {
    format!(
        "a.id IN (SELECT asset_id FROM versions WHERE ai_facts != 0 AND (ai_facts & {bits}) != 0)"
    )
}

fn day(value: &Value) -> Result<chrono::NaiveDate> {
    let text = value.as_str().context("Dates are `YYYY-MM-DD`")?;
    chrono::NaiveDate::parse_from_str(text.get(..10).unwrap_or(text), "%Y-%m-%d")
        .with_context(|| format!("{text} is not a date (`YYYY-MM-DD`)"))
}

/// The first day of the period `{amount, unit}` ending today.
fn since(value: &Value, today: chrono::NaiveDate) -> Result<chrono::NaiveDate> {
    let amount = value
        .get("amount")
        .and_then(Value::as_u64)
        .filter(|n| (1..=10_000).contains(n))
        .context("A period needs an amount of 1–10000")? as u32;
    let unit = value.get("unit").and_then(Value::as_str).unwrap_or("days");
    let months = |n: u32| {
        today
            .checked_sub_months(chrono::Months::new(n))
            .context("Period too long")
    };
    // "In the last 1 day" is today.
    Ok(match unit {
        "days" => today - chrono::Days::new(u64::from(amount) - 1),
        "weeks" => today - chrono::Days::new(u64::from(amount) * 7 - 1),
        "months" => months(amount)? + chrono::Days::new(1),
        "years" => {
            months(amount.checked_mul(12).context("Period too long")?)? + chrono::Days::new(1)
        }
        other => bail!("Unknown period {other}"),
    })
}

/// SQL text for a `LIKE`-free substring test ignoring case
/// (`crate::FOLD`, the same folding as the text index).
fn contains_sql(column: &str) -> String {
    format!("instr(rawtonic_fold({column}), rawtonic_fold(?)) > 0")
}

impl<'a> Compiler<'a> {
    fn new(directory: &'a Path, library: Option<&'a Library>) -> Self {
        Self {
            directory,
            library,
            params: Vec::new(),
            expanding: HashSet::new(),
            expansions: 0,
            today: chrono::Local::now().date_naive(),
            metadata: false,
            roots: false,
            duplicates: false,
            sep: std::path::MAIN_SEPARATOR,
        }
    }

    fn push(&mut self, value: impl Into<Sql>) {
        self.params.push(value.into());
    }

    /// Whether more than half of the photographs hold `phrase` (the index
    /// counts them in a few milliseconds).
    fn common(&self, phrase: &str) -> Result<bool> {
        let Some(library) = self.library else {
            return Ok(false);
        };
        let (matches, all): (i64, i64) = library.conn.query_row(
            "SELECT (SELECT count(*) FROM asset_search WHERE asset_search MATCH ?), (SELECT count(*) FROM assets)",
            [phrase],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok(matches * 2 > all)
    }

    /// `column` starts or ends with `text` (not empty), ignoring case.
    fn affix(&mut self, column: &str, op: Op, text: String) -> Result<String> {
        if op == Op::StartsWith {
            self.push(text);
            Ok(format!(
                "instr(rawtonic_fold({column}), rawtonic_fold(?)) = 1"
            ))
        } else {
            self.push(text.chars().count() as i64);
            self.push(text);
            Ok(format!(
                "rawtonic_fold(substr({column}, -?)) = rawtonic_fold(?)"
            ))
        }
    }

    fn rule(&mut self, rule: &Rule) -> Result<String> {
        match rule {
            Rule::Group { combine, rules } => {
                let parts = rules
                    .iter()
                    .map(|r| self.rule(r))
                    .collect::<Result<Vec<_>>>()?;
                Ok(match (combine, parts.is_empty()) {
                    (Combine::All | Combine::None, true) => "1".into(),
                    (Combine::Any, true) => "0".into(),
                    (Combine::All, false) => format!("({})", parts.join(" AND ")),
                    (Combine::Any, false) => format!("({})", parts.join(" OR ")),
                    (Combine::None, false) => format!("NOT ({})", parts.join(" OR ")),
                })
            }
            Rule::Condition { field, op, value } => self.condition(*field, *op, value),
        }
    }

    /// Words of a text rule against a column: every / any / none of them
    /// part of it.
    fn text_words(&mut self, column: &str, op: Op, value: &Value) -> Result<String> {
        let words = words(value)?;
        if words.is_empty() {
            return Ok(if op == Op::ContainsAny { "0" } else { "1" }.into());
        }
        let test = contains_sql(column);
        let parts: Vec<String> = words
            .into_iter()
            .map(|word| {
                self.push(word);
                test.clone()
            })
            .collect();
        Ok(match op {
            Op::Contains => format!("({})", parts.join(" AND ")),
            Op::ContainsAny => format!("({})", parts.join(" OR ")),
            _ => format!("NOT ({})", parts.join(" OR ")),
        })
    }

    /// Words in any searchable text: a word of three or more characters
    /// through the trigram index (its subquery runs once), a shorter or a
    /// common one by a scan of the same text in each row; both fold case
    /// the same way (`crate::FOLD`), so which one runs never changes what
    /// is found.
    fn text_any(&mut self, op: Op, value: &Value) -> Result<String> {
        let words = words(value)?;
        if words.is_empty() {
            return Ok(if op == Op::ContainsAny { "0" } else { "1" }.into());
        }
        let mut parts = Vec::with_capacity(words.len());
        for word in words {
            // The index holds folded text and compares exactly.
            let phrase = format!("\"{}\"", crate::fold(&word).replace('"', "\"\""));
            // A word most photographs have (the first letters of a file
            // name) is faster to check in each row than to collect from
            // the index; the index finds the others at once.
            if word.chars().count() >= 3 && !self.common(&phrase)? {
                self.push(phrase);
                parts.push(
                    "a.id IN (SELECT asset_id FROM asset_search WHERE asset_search MATCH ?)".into(),
                );
            } else {
                // The text the index holds (migration 0011), read from the
                // photograph's own row.
                self.push(word);
                parts.push(
                    "instr(rawtonic_fold(a.rel_path || ' ' || a.search_text || ' ' || a.make || ' ' || \
                     a.camera || ' ' || a.lens), rawtonic_fold(?)) > 0"
                        .to_string(),
                );
            }
        }
        Ok(match op {
            Op::Contains => format!("({})", parts.join(" AND ")),
            Op::ContainsAny => format!("({})", parts.join(" OR ")),
            _ => format!("NOT ({})", parts.join(" OR ")),
        })
    }

    /// A text column compared as a whole, ignoring case.
    fn text_column(&mut self, column: &str, op: Op, value: &Value) -> Result<String> {
        let coalesced = format!("coalesce({column},'')");
        Ok(match op {
            Op::Contains | Op::ContainsAny | Op::NotContains => {
                self.text_words(&coalesced, op, value)?
            }
            Op::Is | Op::IsNot => {
                self.push(text_value(value)?);
                let test = format!("rawtonic_fold(trim({coalesced})) = rawtonic_fold(?)");
                if op == Op::Is {
                    test
                } else {
                    format!("NOT ({test})")
                }
            }
            Op::StartsWith | Op::EndsWith => {
                let text = text_value(value)?;
                if text.is_empty() {
                    return Ok("1".into());
                }
                self.affix(&coalesced, op, text)?
            }
            Op::Empty => format!("trim({coalesced}) = ''"),
            Op::NotEmpty => format!("trim({coalesced}) != ''"),
            _ => bail!("This operator does not apply to text"),
        })
    }

    /// `number_column_inner`, false (not NULL) for a photograph without the
    /// value: "none of [ISO ≥ 800]" keeps the photographs without an ISO,
    /// as "no rule holds" says. (Other columns are never NULL; wrapping
    /// every condition would keep SQLite from using the text index.)
    fn number_column(
        &mut self,
        column: &str,
        op: Op,
        value: &Value,
        tolerance: Tolerance,
    ) -> Result<String> {
        Ok(format!(
            "coalesce({}, 0)",
            self.number_column_inner(column, op, value, tolerance)?
        ))
    }

    fn number_column_inner(
        &mut self,
        column: &str,
        op: Op,
        value: &Value,
        tolerance: Tolerance,
    ) -> Result<String> {
        let near = |n: f64| match tolerance {
            Tolerance::Absolute(t) => t,
            Tolerance::Relative(r) => n.abs() * r,
            Tolerance::Listed(_) => 0.0,
        };
        Ok(match (op, tolerance) {
            (Op::Is | Op::IsNot, Tolerance::Listed(listed)) => {
                let n = number(value)?;
                self.push(n);
                self.push(n);
                let test = format!("({column} = ? OR {listed} = ?)");
                if op == Op::Is {
                    test
                } else {
                    format!("({column} IS NULL OR NOT {test})")
                }
            }
            (Op::Is | Op::IsNot, _) => {
                let n = number(value)?;
                self.push(n);
                self.push(near(n));
                let test = format!("abs({column} - ?) <= ?");
                if op == Op::Is {
                    test
                } else {
                    format!("({column} IS NULL OR NOT ({test}))")
                }
            }
            (Op::Lt | Op::Le | Op::Gt | Op::Ge, _) => {
                let n = number(value)?;
                // Within the tolerance counts as equal.
                let (sign, bound) = match op {
                    Op::Lt => ("<", n - near(n)),
                    Op::Le => ("<=", n + near(n)),
                    Op::Gt => (">", n + near(n)),
                    _ => (">=", n - near(n)),
                };
                self.push(bound);
                format!("{column} {sign} ?")
            }
            (Op::Between, _) => {
                let (from, to) = pair(value, number)?;
                let (from, to) = if from <= to { (from, to) } else { (to, from) };
                self.push(from - near(from));
                self.push(to + near(to));
                format!("{column} BETWEEN ? AND ?")
            }
            (Op::Empty, _) => format!("{column} IS NULL"),
            (Op::NotEmpty, _) => format!("{column} IS NOT NULL"),
            _ => bail!("This operator does not apply to numbers"),
        })
    }

    /// A date column holding `YYYY-MM-DD HH:MM:SS` text (local time).
    /// `date_column_inner`, false for a photograph without the date (as
    /// `number_column`).
    fn date_column(&mut self, column: &str, op: Op, value: &Value) -> Result<String> {
        Ok(format!(
            "coalesce({}, 0)",
            self.date_column_inner(column, op, value)?
        ))
    }

    fn date_column_inner(&mut self, column: &str, op: Op, value: &Value) -> Result<String> {
        let day_of = format!("substr({column},1,10)");
        let text = |d: chrono::NaiveDate| d.format("%Y-%m-%d").to_string();
        Ok(match op {
            Op::Is | Op::IsNot => {
                self.push(text(day(value)?));
                let test = format!("{day_of} = ?");
                if op == Op::Is {
                    test
                } else {
                    format!("({column} IS NULL OR {day_of} != ?)")
                }
            }
            Op::Lt | Op::Le | Op::Gt | Op::Ge => {
                self.push(text(day(value)?));
                let sign = match op {
                    Op::Lt => "<",
                    Op::Le => "<=",
                    Op::Gt => ">",
                    _ => ">=",
                };
                format!("{day_of} {sign} ?")
            }
            Op::Between => {
                let (from, to) = pair(value, day)?;
                let (from, to) = if from <= to { (from, to) } else { (to, from) };
                self.push(text(from));
                self.push(text(to));
                format!("{day_of} BETWEEN ? AND ?")
            }
            Op::InLast | Op::NotInLast => {
                let from = since(value, self.today)?;
                self.push(text(from));
                self.push(text(self.today));
                let test = format!("{day_of} BETWEEN ? AND ?");
                if op == Op::InLast {
                    test
                } else {
                    format!("({column} IS NULL OR NOT ({test}))")
                }
            }
            Op::Empty => format!("{column} IS NULL"),
            Op::NotEmpty => format!("{column} IS NOT NULL"),
            _ => bail!("This operator does not apply to dates"),
        })
    }

    /// `test` for "is one of `names`", or its negation.
    fn one_of(&mut self, column: &str, op: Op, values: Vec<Sql>) -> Result<String> {
        let marks = vec!["?"; values.len()].join(",");
        self.params.extend(values);
        Ok(match op {
            Op::Is => format!("{column} IN ({marks})"),
            Op::IsNot => format!("{column} NOT IN ({marks})"),
            _ => bail!("Use is / is not"),
        })
    }

    /// The photograph has one of the AI facts `bits` (`ai_test`); those
    /// only RAW files have (`RAW_ONLY`) on a RAW file only.
    fn ai_has(&mut self, bits: u32) -> String {
        let mut parts = Vec::new();
        if bits & !RAW_ONLY != 0 {
            parts.push(ai_test(bits & !RAW_ONLY));
        }
        if bits & RAW_ONLY != 0 {
            parts.push(format!(
                "({} AND {})",
                ai_test(bits & RAW_ONLY),
                self.raw_test()
            ));
        }
        format!("({})", parts.join(" OR "))
    }

    /// The photograph is a RAW file (file type `raw` or `dng`).
    fn raw_test(&mut self) -> String {
        let others: Vec<Sql> = FILE_TYPES
            .iter()
            .filter(|(kind, _)| *kind != "dng")
            .flat_map(|(_, list)| list.iter().map(|e| Sql::Text((*e).into())))
            .collect();
        let marks = vec!["?"; others.len()].join(",");
        self.params.extend(others);
        format!("a.kind NOT IN ({marks})")
    }

    fn condition(&mut self, field: Field, op: Op, value: &Value) -> Result<String> {
        match field {
            Field::Text => match op {
                Op::Contains | Op::ContainsAny | Op::NotContains => self.text_any(op, value),
                _ => bail!("Search text supports contains / contains any / does not contain"),
            },
            Field::Details => match op {
                Op::Contains | Op::ContainsAny | Op::NotContains => {
                    self.metadata = true;
                    // The searchable text (`search_text`) but the keywords,
                    // read from the stored metadata (a row scan).
                    self.text_words(
                        "(a.rel_path || ' ' || a.make || ' ' || a.camera || ' ' || a.lens || ' ' || \
                          coalesce(json_extract(m.data, '$.title'), '') || ' ' || \
                          coalesce(json_extract(m.data, '$.description'), '') || ' ' || \
                          coalesce(json_extract(m.data, '$.creator'), '') || ' ' || \
                          coalesce((SELECT group_concat(value, ' ') FROM json_each(m.data, '$.iptc')), ''))",
                        op,
                        value,
                    )
                }
                _ => bail!("Search text supports contains / contains any / does not contain"),
            },
            Field::Filename => {
                // The name after the last `/` of the path in its root (the
                // stored separator on every platform).
                self.text_column(
                    "replace(a.rel_path, rtrim(a.rel_path, replace(a.rel_path, '/', '')), '')",
                    op,
                    value,
                )
            }
            Field::Camera => self.text_column("a.camera", op, value),
            Field::Make => self.text_column("a.make", op, value),
            Field::Lens => self.text_column("a.lens", op, value),
            Field::Keyword => match op {
                Op::Is | Op::IsNot => {
                    self.push(text_value(value)?);
                    let test = "EXISTS(SELECT 1 FROM asset_keywords k WHERE k.asset_id=a.id \
                                AND rawtonic_fold(k.keyword)=rawtonic_fold(?))";
                    Ok(if op == Op::Is {
                        test.into()
                    } else {
                        format!("NOT {test}")
                    })
                }
                Op::Contains
                | Op::ContainsAny
                | Op::NotContains
                | Op::StartsWith
                | Op::EndsWith => {
                    // Words against each keyword: a photograph matches when
                    // one of its keywords does.
                    let inner = match op {
                        Op::StartsWith | Op::EndsWith => {
                            let text = text_value(value)?;
                            if text.is_empty() {
                                return Ok("1".into());
                            }
                            self.affix("k.keyword", op, text)?
                        }
                        Op::NotContains => {
                            let positive = self.text_words("k.keyword", Op::ContainsAny, value)?;
                            return Ok(format!(
                                "NOT EXISTS(SELECT 1 FROM asset_keywords k WHERE k.asset_id=a.id AND {positive})"
                            ));
                        }
                        _ => {
                            // Contains: every word in some keyword (not
                            // necessarily the same one); any: one word.
                            let words = words(value)?;
                            if words.is_empty() {
                                return Ok(if op == Op::ContainsAny { "0" } else { "1" }.into());
                            }
                            let parts: Vec<String> = words
                                .into_iter()
                                .map(|w| {
                                    self.push(w);
                                    format!(
                                        "EXISTS(SELECT 1 FROM asset_keywords k WHERE k.asset_id=a.id AND {})",
                                        contains_sql("k.keyword")
                                    )
                                })
                                .collect();
                            let join = if op == Op::Contains { " AND " } else { " OR " };
                            return Ok(format!("({})", parts.join(join)));
                        }
                    };
                    Ok(format!(
                        "EXISTS(SELECT 1 FROM asset_keywords k WHERE k.asset_id=a.id AND {inner})"
                    ))
                }
                Op::Empty => {
                    Ok("NOT EXISTS(SELECT 1 FROM asset_keywords k WHERE k.asset_id=a.id)".into())
                }
                Op::NotEmpty => {
                    Ok("EXISTS(SELECT 1 FROM asset_keywords k WHERE k.asset_id=a.id)".into())
                }
                _ => bail!("This operator does not apply to keywords"),
            },
            Field::FileType => {
                let kinds = names(value)?;
                let mut extensions = Vec::new();
                let mut raw = false;
                for kind in &kinds {
                    if kind == "raw" {
                        raw = true;
                    } else {
                        let (_, list) = FILE_TYPES
                            .iter()
                            .find(|(k, _)| k == kind)
                            .with_context(|| format!("Unknown file type {kind}"))?;
                        extensions.extend(list.iter().map(|e| Sql::Text((*e).into())));
                    }
                }
                let mut parts = Vec::new();
                if !extensions.is_empty() {
                    parts.push(self.one_of("a.kind", Op::Is, extensions)?);
                }
                if raw {
                    let all: Vec<Sql> = FILE_TYPES
                        .iter()
                        .flat_map(|(_, list)| list.iter().map(|e| Sql::Text((*e).into())))
                        .collect();
                    parts.push(self.one_of("a.kind", Op::IsNot, all)?);
                }
                let test = format!("({})", parts.join(" OR "));
                match op {
                    Op::Is => Ok(test),
                    Op::IsNot => Ok(format!("NOT {test}")),
                    _ => bail!("Use is / is not for file types"),
                }
            }
            Field::Rating | Field::Flag | Field::Label if !self.metadata => {
                self.metadata = true;
                self.condition(field, op, value)
            }
            Field::Rating => match op {
                Op::Empty => Ok("m.rating = 0".into()),
                Op::NotEmpty => Ok("m.rating > 0".into()),
                _ => {
                    let check = |n: f64| (0.0..=5.0).contains(&n);
                    match op {
                        Op::Between => ensure!(
                            pair(value, number).is_ok_and(|(a, b)| check(a) && check(b)),
                            "Ratings are 0–5"
                        ),
                        _ => ensure!(number(value).is_ok_and(check), "Ratings are 0–5"),
                    }
                    // Never NULL (0 is unrated).
                    self.number_column_inner("m.rating", op, value, Tolerance::Absolute(0.0))
                }
            },
            Field::Flag => {
                let flags = names(value)?
                    .iter()
                    .map(|name| match name.as_str() {
                        "picked" => Ok(Sql::Integer(1)),
                        "rejected" => Ok(Sql::Integer(-1)),
                        "unflagged" => Ok(Sql::Integer(0)),
                        other => bail!("Unknown flag {other}"),
                    })
                    .collect::<Result<_>>()?;
                self.one_of("m.flag", op, flags)
            }
            Field::Label => match op {
                Op::Empty => Ok("m.label = ''".into()),
                Op::NotEmpty => Ok("m.label != ''".into()),
                _ => {
                    let labels = names(value)?
                        .into_iter()
                        .map(|name| match name.as_str() {
                            "none" => Ok(Sql::Text(String::new())),
                            known if LABELS.contains(&known) => Ok(Sql::Text(name)),
                            other => bail!("Unknown label {other}"),
                        })
                        .collect::<Result<_>>()?;
                    self.one_of("lower(m.label)", op, labels)
                }
            },
            Field::Iso => self.number_column("a.iso", op, value, Tolerance::Absolute(0.0)),
            // "Is" also takes the photographs the value lists count under
            // the value (`Tally::add`: whole millimetres, f-numbers to a
            // tenth), so a listed value selects exactly what it counts;
            // an exact value (50.5) and comparisons read the stored one.
            Field::FocalLength => self.number_column(
                "a.focal_length",
                op,
                value,
                Tolerance::Listed("round(a.focal_length)"),
            ),
            Field::Aperture => self.number_column(
                "a.aperture",
                op,
                value,
                Tolerance::Listed("(round(a.aperture * 10) / 10.0)"),
            ),
            // Cameras round exposure times (1/250 is 0.004 or 1/256).
            Field::Exposure => {
                self.number_column("a.exposure", op, value, Tolerance::Relative(0.03))
            }
            Field::CaptureDate => self.date_column(crate::capture_time::EFFECTIVE, op, value),
            Field::ImportDate => self.date_column(
                "(SELECT datetime(b.created_at,'localtime') FROM asset_imports i \
                  JOIN import_batches b ON b.id=i.batch_id WHERE i.asset_id=a.id)",
                op,
                value,
            ),
            Field::Offline => {
                let missing = value.as_bool().context("Offline is true or false")?;
                match op {
                    Op::Is => Ok(format!("a.missing = {}", i32::from(missing))),
                    Op::IsNot => Ok(format!("a.missing != {}", i32::from(missing))),
                    _ => bail!("Use is / is not for offline"),
                }
            }
            Field::PreviousImport => {
                let wanted = value
                    .as_bool()
                    .context("Previous import is true or false")?;
                let test = "a.id IN (SELECT asset_id FROM asset_imports \
                            WHERE batch_id=(SELECT max(id) FROM import_batches))";
                match (op, wanted) {
                    (Op::Is, true) | (Op::IsNot, false) => Ok(test.into()),
                    (Op::Is, false) | (Op::IsNot, true) => Ok(format!("NOT {test}")),
                    _ => bail!("Use is / is not for previous import"),
                }
            }
            Field::Duplicate => {
                self.duplicates = true;
                let wanted = value.as_bool().context("Duplicate is true or false")?;
                let others = self
                    .library
                    .map_or("a.trashed=0", |library| library.visible_assets())
                    .replace("a.", "d.");
                let test = format!(
                    "(a.missing=0 AND EXISTS (SELECT 1 FROM image_hashes h \
                      JOIN image_hashes o ON o.hash=h.hash AND o.asset_id!=h.asset_id \
                      JOIN assets d ON d.id=o.asset_id \
                      WHERE h.asset_id=a.id AND d.missing=0 AND {others}))"
                );
                match (op, wanted) {
                    (Op::Is, true) | (Op::IsNot, false) => Ok(test),
                    (Op::Is, false) | (Op::IsNot, true) => Ok(format!("NOT {test}")),
                    _ => bail!("Use is / is not for duplicate"),
                }
            }
            Field::Ai => {
                let test = match op {
                    Op::Is | Op::IsNot => {
                        let mut bits = 0;
                        let mut none = false;
                        for name in names(value)? {
                            if name == "none" {
                                none = true;
                            } else {
                                bits |= ai_fact(&name)?;
                            }
                        }
                        let mut parts = Vec::new();
                        if none {
                            parts.push(format!("NOT {}", self.ai_has(AI_ANY)));
                        }
                        if bits != 0 {
                            parts.push(self.ai_has(bits));
                        }
                        format!("({})", parts.join(" OR "))
                    }
                    Op::Empty => format!("NOT {}", self.ai_has(AI_ANY)),
                    Op::NotEmpty => self.ai_has(AI_ANY),
                    _ => bail!("Use is / is not for AI edits"),
                };
                Ok(if op == Op::IsNot {
                    format!("NOT {test}")
                } else {
                    test
                })
            }
            Field::SourceType => match op {
                Op::Is | Op::IsNot => {
                    let codes = names_verbatim(value)?
                        .into_iter()
                        .map(|code| {
                            Sql::Text(if code.eq_ignore_ascii_case("none") {
                                String::new()
                            } else {
                                rawtonic_metadata::provenance::source_type_code(&code)
                            })
                        })
                        .collect();
                    self.one_of("a.source_type", op, codes)
                }
                _ => self.text_column("a.source_type", op, value),
            },
            Field::Album => {
                let id = text_value(value)?;
                let test = self.album(&id)?;
                match op {
                    Op::Contains | Op::Is => Ok(test),
                    Op::NotContains | Op::IsNot => Ok(format!("NOT {test}")),
                    _ => bail!("Use in / not in for albums"),
                }
            }
            Field::Folder => {
                // A folder of the library is kept relative to it (smart
                // albums move with a session); others are absolute.
                // Verbatim: `Day ` and `Day` are two folders.
                let folder = value.as_str().context("Folders are paths")?;
                ensure!(
                    !folder.is_empty() && folder.len() <= 4096,
                    "A folder rule needs a path"
                );
                let folder = PathBuf::from(folder);
                let folder = if folder.is_absolute() {
                    folder
                } else if folder == Path::new(".") {
                    self.directory.to_path_buf()
                } else {
                    ensure!(
                        folder
                            .components()
                            .all(|c| matches!(c, std::path::Component::Normal(_))),
                        "A folder of the library cannot leave it"
                    );
                    self.directory.join(folder)
                };
                let test = self.folder(&folder, op != Op::Is && op != Op::IsNot)?;
                match op {
                    Op::Contains | Op::Is => Ok(test),
                    Op::NotContains | Op::IsNot => Ok(format!("NOT {test}")),
                    _ => bail!("Use in / not in for folders"),
                }
            }
        }
    }

    /// Photographs of an album, of every album in a group, or of a smart
    /// album's rules.
    fn album(&mut self, id: &str) -> Result<String> {
        let Some(library) = self.library else {
            return Ok("0".into());
        };
        let row: Option<(String, Option<String>)> =
            rusqlite::OptionalExtension::optional(library.conn.query_row(
                "SELECT kind, rules FROM collections WHERE id=?",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            ))?;
        Ok(match row {
            None => "0".into(),
            Some((kind, _)) if kind == crate::ALBUM => {
                self.push(id.to_string());
                "a.id IN (SELECT asset_id FROM collection_assets WHERE collection_id=?)".into()
            }
            Some((kind, _)) if kind == crate::GROUP => {
                // A group being expanded (a smart album in it naming it)
                // adds nothing more there.
                if !self.expand(id)? {
                    return Ok("0".into());
                }
                // Every album and smart album below the group.
                let children: Vec<String> = library
                    .conn
                    .prepare(
                        "WITH RECURSIVE below(id) AS (SELECT id FROM collections WHERE parent=?
                           UNION SELECT c.id FROM collections c JOIN below b ON c.parent=b.id)
                         SELECT c.id FROM collections c JOIN below b ON b.id=c.id WHERE c.kind!='group'",
                    )?
                    .query_map([id], |r| r.get(0))?
                    .collect::<rusqlite::Result<_>>()?;
                let parts = children
                    .iter()
                    .map(|child| self.album(child))
                    .collect::<Result<Vec<_>>>()?;
                self.expanding.remove(id);
                if parts.is_empty() {
                    "0".into()
                } else {
                    format!("({})", parts.join(" OR "))
                }
            }
            Some((_, rules)) => {
                if !self.expand(id)? {
                    return Ok("0".into());
                }
                let rule: Rule = serde_json::from_str(rules.as_deref().unwrap_or("{}"))
                    .context("A smart album's rules are damaged")?;
                let sql = self.rule(&rule)?;
                self.expanding.remove(id);
                sql
            }
        })
    }

    /// Starts expanding group or smart album `id`: false when it is being
    /// expanded already (a loop). Refuses nesting deeper than `MAX_DEPTH`
    /// and more than `MAX_EXPANSIONS` expansions in one rule, so smart
    /// albums naming each other can never build an unbounded query.
    fn expand(&mut self, id: &str) -> Result<bool> {
        ensure!(
            self.expanding.len() < MAX_DEPTH,
            "Smart albums refer to each other too deeply"
        );
        self.expansions += 1;
        ensure!(
            self.expansions <= MAX_EXPANSIONS,
            "Smart albums refer to each other too often"
        );
        Ok(self.expanding.insert(id.to_string()))
    }

    /// Photographs in `folder` (and its subfolders when `below`): those of
    /// the library's own root by their path in it, those of external roots
    /// by the root's folder.
    fn folder(&mut self, folder: &Path, below: bool) -> Result<String> {
        // Photographs are registered by their real place: a folder reached
        // through a symlink is the folder it points to.
        let real = folder.canonicalize();
        let folder = real.as_deref().unwrap_or(folder);
        let sep = self.sep;
        // Without trailing separators, but a root (`/`, `C:\`) as it is.
        let text = |p: &Path| {
            let full = p.to_string_lossy();
            match full.trim_end_matches(sep) {
                trimmed if trimmed.is_empty() || trimmed.ends_with(':') => full.into_owned(),
                trimmed => trimmed.to_string(),
            }
        };
        // Library root: the folder relative to the library directory. A
        // library's photographs store their path in it with `/`, whatever
        // the platform, so the rule asks for that form.
        let own = match folder.strip_prefix(self.directory) {
            Ok(relative) => {
                let relative = if relative.as_os_str().is_empty() {
                    String::new()
                } else {
                    crate::portable_relative(relative)?
                };
                let directory_of = "rtrim(a.rel_path, replace(a.rel_path, '/', ''))";
                if relative.is_empty() {
                    if below {
                        "a.root_id='session'".to_string()
                    } else {
                        format!("(a.root_id='session' AND {directory_of} = '')")
                    }
                } else if below {
                    self.push(format!("{relative}/"));
                    "(a.root_id='session' AND instr(a.rel_path, ?) = 1)".to_string()
                } else {
                    self.push(format!("{relative}/"));
                    format!("(a.root_id='session' AND {directory_of} = ?)")
                }
            }
            // A folder holding the library: with its subfolders, every
            // photograph in the library's own folder.
            Err(_) if below && self.directory.starts_with(folder) => {
                "a.root_id='session'".to_string()
            }
            Err(_) => "0".to_string(),
        };
        self.roots = true;
        let folder = text(folder);
        let external = if below {
            self.push(folder.clone());
            self.push(if folder.ends_with(sep) {
                folder.clone()
            } else {
                format!("{folder}{sep}")
            });
            "(r.external=1 AND (r.path = ? OR instr(r.path, ?) = 1))".to_string()
        } else {
            self.push(folder);
            "(r.external=1 AND r.path = ?)".to_string()
        };
        Ok(format!("({own} OR {external})"))
    }
}

#[derive(Clone, Copy)]
enum Tolerance {
    Absolute(f64),
    Relative(f64),
    /// Exact, and for "is" also the photographs a value list counts under
    /// the value (the SQL that rounds as the list does).
    Listed(&'static str),
}

/// One value of a facet and how many photographs have it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FacetValue {
    /// The value as stored (`""`: none).
    pub value: String,
    pub count: usize,
}

/// Fields the Library filter lists the values of (Lightroom's metadata
/// columns, Capture One's Filters tool); capture dates by month
/// (`YYYY-MM`).
pub const FACETS: &[Field] = &[
    Field::CaptureDate,
    Field::Camera,
    Field::Lens,
    Field::FileType,
    Field::Keyword,
    Field::Rating,
    Field::Flag,
    Field::Label,
    Field::Iso,
    Field::FocalLength,
    Field::Aperture,
    Field::Ai,
    Field::SourceType,
];

/// A compiled rule: its condition and parameters, and the tables it needs
/// besides `assets a` (joining them for every photograph costs as much as
/// the search itself).
struct Compiled {
    condition: String,
    params: Vec<Sql>,
    metadata: bool,
    roots: bool,
}

impl Compiled {
    /// `FROM` with the joins the condition (and `select`) need.
    fn from(&self, metadata: bool, roots: bool) -> String {
        let mut from = "assets a".to_string();
        if self.metadata || metadata {
            from.push_str(" JOIN metadata m ON m.asset_id=a.id");
        }
        if self.roots || roots {
            from.push_str(" JOIN roots r ON r.id=a.root_id");
        }
        from
    }

    fn params(&self) -> Vec<&dyn ToSql> {
        self.params.iter().map(|p| p as &dyn ToSql).collect()
    }
}

/// The column of a field in the facet scan (`Library::facets`).
fn facet_column(field: Field) -> Option<usize> {
    Some(match field {
        Field::CaptureDate => 0,
        Field::Camera => 1,
        Field::Lens => 2,
        Field::FileType => 3,
        Field::Iso => 4,
        Field::FocalLength => 5,
        Field::Aperture => 6,
        Field::Rating => 7,
        Field::Flag => 8,
        Field::Label => 9,
        Field::SourceType => 10,
        _ => return None,
    })
}

/// One facet's counts during the scan, by the value as stored (no
/// allocation per photograph); `values` turns them into what is listed.
#[derive(Default)]
struct Tally {
    texts: HashMap<String, usize>,
    numbers: HashMap<u64, usize>,
    none: usize,
}

impl Tally {
    fn add(&mut self, field: Field, value: ValueRef<'_>) {
        match value {
            ValueRef::Null => self.none += 1,
            ValueRef::Integer(n) => *self.numbers.entry((n as f64).to_bits()).or_default() += 1,
            ValueRef::Real(x) => {
                // Listed as photographers read them: 50 mm, f/2.8.
                let x = match field {
                    Field::FocalLength => x.round(),
                    Field::Aperture => (x * 10.0).round() / 10.0,
                    _ => x,
                };
                *self.numbers.entry(x.to_bits()).or_default() += 1;
            }
            ValueRef::Text(bytes) => {
                let text = std::str::from_utf8(bytes).unwrap_or("").trim();
                if text.is_empty() {
                    self.none += 1;
                } else if let Some(n) = self.texts.get_mut(text) {
                    *n += 1;
                } else {
                    self.texts.insert(text.to_string(), 1);
                }
            }
            ValueRef::Blob(_) => {}
        }
    }

    /// The listed values: most frequent first, numbers ascending, months
    /// newest first, `""` (none) last among equals.
    fn values(self, field: Field) -> Vec<FacetValue> {
        let mut merged: HashMap<String, usize> = HashMap::new();
        // Spellings that differ only in case are one value, as the camera
        // and lens rules compare (`crate::fold`); the most frequent
        // spelling is shown.
        let mut spelled: HashMap<String, (String, usize)> = HashMap::new();
        for (text, n) in self.texts {
            let key = match field {
                Field::FileType => file_type(&text.to_lowercase()).to_string(),
                Field::Label => text.to_lowercase(),
                Field::Camera | Field::Lens => {
                    let folded = crate::fold(&text);
                    let shown = spelled.entry(folded.clone()).or_insert((text.clone(), 0));
                    if n > shown.1 || (n == shown.1 && text < shown.0) {
                        *shown = (text, n);
                    }
                    folded
                }
                _ => text,
            };
            *merged.entry(key).or_default() += n;
        }
        let mut merged: HashMap<String, usize> = merged
            .into_iter()
            .map(|(key, n)| match spelled.remove(&key) {
                Some((shown, _)) => (shown, n),
                None => (key, n),
            })
            .collect();
        for (bits, n) in self.numbers {
            let x = f64::from_bits(bits);
            let key = match field {
                Field::Flag => match x as i64 {
                    1 => "picked",
                    -1 => "rejected",
                    _ => "unflagged",
                }
                .to_string(),
                _ => trim_number(x),
            };
            *merged.entry(key).or_default() += n;
        }
        if self.none > 0 {
            // An extensionless file is a RAW to the search too.
            let key = if field == Field::FileType {
                file_type("")
            } else {
                ""
            };
            *merged.entry(key.to_string()).or_default() += self.none;
        }
        let mut list: Vec<FacetValue> = merged
            .into_iter()
            .map(|(value, count)| FacetValue { value, count })
            .collect();
        let number = |v: &FacetValue| v.value.parse::<f64>().ok();
        match field {
            Field::CaptureDate => list.sort_by(|a, b| {
                (a.value.is_empty(), &b.value).cmp(&(b.value.is_empty(), &a.value))
            }),
            Field::Iso | Field::FocalLength | Field::Aperture | Field::Rating => {
                list.sort_by(|a, b| match (number(a), number(b)) {
                    (Some(x), Some(y)) => x.total_cmp(&y),
                    (x, y) => y.is_some().cmp(&x.is_some()),
                })
            }
            _ => list.sort_by(|a, b| {
                b.count
                    .cmp(&a.count)
                    .then_with(|| a.value.is_empty().cmp(&b.value.is_empty()))
                    .then_with(|| a.value.to_lowercase().cmp(&b.value.to_lowercase()))
            }),
        }
        list
    }
}

impl Library {
    fn compile(&self, rule: &Rule) -> Result<Compiled> {
        self.compile_visible(rule, self.visible_assets())
    }

    /// Whether `rule`, with the smart albums it names, compares image data
    /// (`Field::Duplicate`): its answer needs current image hashes.
    pub fn rule_compares_images(&self, rule: &Rule) -> Result<bool> {
        rule.validate()?;
        let mut compiler = Compiler::new(&self.directory, Some(self));
        compiler.rule(rule)?;
        Ok(compiler.duplicates)
    }

    fn compile_visible(&self, rule: &Rule, visible: &str) -> Result<Compiled> {
        rule.validate()?;
        let mut compiler = Compiler::new(&self.directory, Some(self));
        let condition = format!("({visible}) AND ({})", compiler.rule(rule)?);
        Ok(Compiled {
            condition,
            params: compiler.params,
            metadata: compiler.metadata,
            roots: compiler.roots,
        })
    }

    /// Paths of the photographs `rule` matches, by place (root, then path
    /// in it). One query; no file is read.
    pub fn search(&self, rule: &Rule) -> Result<Vec<PathBuf>> {
        self.search_for_listing(rule, None)
    }

    /// Search with an explicit folder-grid context. A listing inside the
    /// session Trash keeps its catalog metadata available for filters.
    pub fn search_for_listing(&self, rule: &Rule, listed: Option<&Path>) -> Result<Vec<PathBuf>> {
        let compiled = self.compile_visible(rule, self.visible_assets_for_listing(listed))?;
        let mut query = self.conn.prepare(&format!(
            "SELECT a.rel_path,r.path,r.external FROM {} WHERE {} ORDER BY a.root_id, a.rel_path",
            compiled.from(false, true),
            compiled.condition
        ))?;
        let rows = query.query_map(compiled.params().as_slice(), |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, bool>(2)?,
            ))
        })?;
        rows.map(|row| {
            let (relative, root, external) = row?;
            Ok(self.join_root(root, external, relative))
        })
        .collect()
    }

    /// Ids of the photographs `rule` matches, in the order of `search`.
    pub fn search_ids(&self, rule: &Rule) -> Result<Vec<String>> {
        self.ids_from_compiled(self.compile(rule)?)
    }

    pub(crate) fn registered_search_ids(&self, rule: &Rule) -> Result<Vec<String>> {
        self.ids_from_compiled(self.compile_visible(rule, "1")?)
    }

    fn ids_from_compiled(&self, compiled: Compiled) -> Result<Vec<String>> {
        let mut query = self.conn.prepare(&format!(
            "SELECT a.id FROM {} WHERE {} ORDER BY a.root_id, a.rel_path",
            compiled.from(false, false),
            compiled.condition
        ))?;
        Ok(query
            .query_map(compiled.params().as_slice(), |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?)
    }

    /// How many photographs `rule` matches.
    pub fn search_count(&self, rule: &Rule) -> Result<usize> {
        let compiled = self.compile(rule)?;
        Ok(self.conn.query_row(
            &format!(
                "SELECT count(*) FROM {} WHERE {}",
                compiled.from(false, false),
                compiled.condition
            ),
            compiled.params().as_slice(),
            |r| r.get::<_, i64>(0),
        )? as usize)
    }

    /// The values of each of `fields` among the photographs `rule` matches,
    /// with their counts: most frequent first, numbers ascending, months
    /// newest first; `""` counts the photographs without a value. A
    /// photograph counts once per keyword it has. One scan of the matches
    /// for every field but keywords, one query for those.
    pub fn facets(
        &self,
        rule: &Rule,
        fields: &[Field],
    ) -> Result<BTreeMap<Field, Vec<FacetValue>>> {
        self.facets_for_listing(rule, fields, None)
    }

    /// The same value lists as `facets`, with the folder-grid visibility
    /// used by `search_for_listing` (including session Trash when shown).
    pub fn facets_for_listing(
        &self,
        rule: &Rule,
        fields: &[Field],
        listed: Option<&Path>,
    ) -> Result<BTreeMap<Field, Vec<FacetValue>>> {
        let compiled = self.compile_visible(rule, self.visible_assets_for_listing(listed))?;
        for field in fields {
            ensure!(FACETS.contains(field), "No value list for {field:?}");
        }
        let mut tallies: Vec<(Field, usize, Tally)> = fields
            .iter()
            .filter_map(|f| facet_column(*f).map(|column| (*f, column, Tally::default())))
            .collect();
        let culling = fields
            .iter()
            .any(|f| matches!(f, Field::Rating | Field::Flag | Field::Label));
        if !tallies.is_empty() {
            let mut query = self.conn.prepare(&format!(
                "SELECT substr({},1,7), a.camera, a.lens, a.kind, a.iso, a.focal_length, a.aperture, {},
                        a.source_type
                 FROM {} WHERE {}",
                crate::capture_time::EFFECTIVE,
                if culling { "m.rating, m.flag, m.label" } else { "0, 0, ''" },
                compiled.from(culling, false),
                compiled.condition
            ))?;
            let mut rows = query.query(compiled.params().as_slice())?;
            while let Some(row) = rows.next()? {
                for (field, column, tally) in tallies.iter_mut() {
                    tally.add(*field, row.get_ref(*column)?);
                }
            }
        }
        let mut result: BTreeMap<Field, Vec<FacetValue>> = tallies
            .into_iter()
            .map(|(field, _, tally)| (field, tally.values(field)))
            .collect();
        if fields.contains(&Field::Keyword) {
            // Keywords differing only in case are one value (folded as the
            // keyword rules compare), with one of their spellings. CROSS JOIN
            // keeps the matches as the outer loop: SQLite would
            // otherwise walk every keyword and look its photograph up
            // (10× slower for a narrow search, measured on 50,000).
            let mut query = self.conn.prepare(&format!(
                "SELECT min(k.keyword), count(DISTINCT a.id) AS n FROM {} CROSS JOIN asset_keywords k ON k.asset_id=a.id
                 WHERE {} GROUP BY rawtonic_fold(k.keyword) ORDER BY n DESC, rawtonic_fold(k.keyword)",
                compiled.from(false, false),
                compiled.condition
            ))?;
            let mut values: Vec<FacetValue> = query
                .query_map(compiled.params().as_slice(), |r| {
                    Ok(FacetValue {
                        value: r.get(0)?,
                        count: r.get::<_, i64>(1)? as usize,
                    })
                })?
                .collect::<rusqlite::Result<_>>()?;
            // And the photographs without any ("No keyword", the `empty` rule).
            let without: i64 = self.conn.query_row(
                &format!(
                    "SELECT count(*) FROM {} WHERE ({}) AND NOT EXISTS(SELECT 1 FROM asset_keywords k WHERE k.asset_id=a.id)",
                    compiled.from(false, false),
                    compiled.condition
                ),
                compiled.params().as_slice(),
                |r| r.get(0),
            )?;
            if without > 0 {
                values.push(FacetValue {
                    value: String::new(),
                    count: without as usize,
                });
            }
            result.insert(Field::Keyword, values);
        }
        if fields.contains(&Field::Ai) {
            result.insert(Field::Ai, self.ai_facet(&compiled)?);
        }
        Ok(result)
    }

    /// The AI facts of asset `id`'s versions (its recipe and its virtual
    /// copies'), together, as indexed (`Field::Ai`).
    pub fn ai_facts(&self, id: &str) -> Result<u32> {
        let mut query = self
            .conn
            .prepare_cached("SELECT ai_facts FROM versions WHERE asset_id=? AND ai_facts != 0")?;
        let mut facts = 0;
        for bits in query.query_map([id], |r| r.get::<_, i64>(0))? {
            facts |= bits? as u32;
        }
        Ok(facts)
    }

    /// The AI facts among the matches of `compiled` (`Field::Ai`), in the
    /// order of `AI_FACTS`, each with how many photographs have it (their
    /// recipe or a virtual copy's; `RAW_ONLY` facts on RAW files only, as
    /// the rule), and `""` for those without any.
    fn ai_facet(&self, compiled: &Compiled) -> Result<Vec<FacetValue>> {
        let mut query = self.conn.prepare(&format!(
            "SELECT a.id, v.ai_facts, a.kind FROM {} JOIN versions v ON v.asset_id=a.id AND v.ai_facts != 0
             WHERE {}",
            compiled.from(false, false),
            compiled.condition
        ))?;
        let mut facts: HashMap<String, u32> = HashMap::new();
        let mut rows = query.query(compiled.params().as_slice())?;
        while let Some(row) = rows.next()? {
            let mut bits = row.get::<_, i64>(1)? as u32;
            let kind: String = row.get(2)?;
            if !matches!(file_type(&kind), "raw" | "dng") {
                bits &= !RAW_ONLY;
            }
            if bits != 0 {
                *facts.entry(row.get(0)?).or_default() |= bits;
            }
        }
        let mut values: Vec<FacetValue> = AI_FACTS
            .iter()
            .map(|fact| FacetValue {
                value: fact.name.to_string(),
                count: facts.values().filter(|bits| *bits & fact.bit != 0).count(),
            })
            .filter(|value| value.count > 0)
            .collect();
        let all: i64 = self.conn.query_row(
            &format!(
                "SELECT count(*) FROM {} WHERE {}",
                compiled.from(false, false),
                compiled.condition
            ),
            compiled.params().as_slice(),
            |r| r.get(0),
        )?;
        let without = (all as usize).saturating_sub(facts.len());
        if without > 0 {
            values.push(FacetValue {
                value: String::new(),
                count: without,
            });
        }
        Ok(values)
    }
}

/// `2.8` → "2.8", `50.0` → "50".
fn trim_number(value: f64) -> String {
    let text = format!("{value:.1}");
    text.strip_suffix(".0").unwrap_or(&text).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Mode;

    /// A photograph directly in the file system's root is in "/" (the
    /// root keeps its separator).
    #[cfg(unix)]
    #[test]
    fn the_root_folder_holds_what_is_directly_in_it() {
        let d = tempfile::tempdir().unwrap();
        let lib = Library::create(&d.path().join("Main"), "Main", Mode::Catalog).unwrap();
        let photo = d.path().join("A.ARW");
        std::fs::write(&photo, b"a").unwrap();
        lib.add_asset(&photo).unwrap();
        // As if it were registered at /A.ARW.
        lib.conn
            .execute("UPDATE roots SET path='/' WHERE external=1", [])
            .unwrap();
        let count = |op: &str| {
            lib.search_count(
                &serde_json::from_value(serde_json::json!({"field":"folder","op":op,"value":"/"}))
                    .unwrap(),
            )
            .unwrap()
        };
        assert_eq!(count("is"), 1);
        assert_eq!(count("contains"), 1);
    }

    /// A folder rule takes a native path (on Windows `C:\\Job\\Capture`) and
    /// a library's photographs store `Capture/…` whatever the platform; an
    /// external root's folder stays the platform's own. File name rules
    /// split on the stored `/`.
    #[test]
    fn folder_and_file_name_rules_read_stored_paths_with_forward_slashes() {
        let d = tempfile::tempdir().unwrap();
        let lib = Library::create(&d.path().join("Job"), "Job", Mode::Session).unwrap();
        // Written with `Path::join`, as a native path reaches a folder.
        for relative in ["Capture/A.ARW", "Capture/Day 2/B.ARW", "Selects/C.ARW"] {
            let photo = relative
                .split('/')
                .fold(lib.directory.clone(), |path, part| path.join(part));
            std::fs::create_dir_all(photo.parent().unwrap()).unwrap();
            std::fs::write(&photo, b"x").unwrap();
            lib.add_asset(&photo).unwrap();
        }
        let mut rows: Vec<String> = lib
            .assets(None)
            .unwrap()
            .into_iter()
            .map(|asset| asset.rel_path)
            .collect();
        rows.sort();
        assert_eq!(
            rows,
            ["Capture/A.ARW", "Capture/Day 2/B.ARW", "Selects/C.ARW"]
        );
        let count = |rule: serde_json::Value| -> i64 {
            let rule: Rule = serde_json::from_value(rule).unwrap();
            let mut compiler = Compiler::new(&lib.directory, Some(&lib));
            // The separator a Windows folder path comes with.
            compiler.sep = '\\';
            let condition = compiler.rule(&rule).unwrap();
            let params: Vec<&dyn ToSql> = compiler.params.iter().map(|p| p as &dyn ToSql).collect();
            lib.conn
                .query_row(
                    &format!("SELECT count(*) FROM assets a JOIN metadata m ON m.asset_id=a.id JOIN roots r ON r.id=a.root_id WHERE {condition}"),
                    params.as_slice(),
                    |r| r.get(0),
                )
                .unwrap()
        };
        let folder = |name: &str| {
            let path = lib.directory.canonicalize().unwrap();
            name.split('/')
                .fold(path, |path, part| path.join(part))
                .to_string_lossy()
                .into_owned()
        };
        let in_folder = |op: &str, name: &str| {
            count(serde_json::json!({"field":"folder","op":op,"value":folder(name)}))
        };
        // Each folder's own photographs; `Selects` is another folder.
        assert_eq!(in_folder("is", "Capture"), 1);
        assert_eq!(in_folder("is", "Capture/Day 2"), 1);
        assert_eq!(in_folder("is", "Selects"), 1);
        // With the subfolders below.
        assert_eq!(in_folder("contains", "Capture"), 2);
        assert_eq!(in_folder("contains", "Capture/Day 2"), 1);
        // File names split on the stored `/`, never on the folder's name.
        assert_eq!(
            count(serde_json::json!({"field":"filename","op":"is","value":"b.arw"})),
            1
        );
        assert_eq!(
            count(serde_json::json!({"field":"filename","op":"startsWith","value":"Capture"})),
            0
        );
    }
}
