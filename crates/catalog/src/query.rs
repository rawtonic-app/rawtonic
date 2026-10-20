//! Library search: one rule model for the Library's filter and for smart
//! albums (G36/G37). A rule is a condition on one field of a photograph
//! (text, keywords, rating, flag, label, file type, camera, lens, exposure,
//! capture or import date, album, folder, offline) or a group of rules that
//! must all, any or none hold, nested as deep as needed: Lightroom's smart
//! collection rules and Capture One's smart album and filter criteria by
//! their documented capability, as one tool. Rules are compiled to one SQL
//! query over the catalog (no file is read), so a smart album is always
//! current: it is evaluated whenever it is shown or counted.
use crate::Library;
use anyhow::{Context, Result, bail, ensure};
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
    pub fn relative_to(&self, directory: &Path) -> Self {
        match self {
            Self::Group { combine, rules } => Self::Group {
                combine: *combine,
                rules: rules.iter().map(|r| r.relative_to(directory)).collect(),
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
                        relative.to_string_lossy().into_owned()
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
        }
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
    today: chrono::NaiveDate,
    /// The condition reads `metadata m` / `roots r`.
    metadata: bool,
    roots: bool,
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
            today: chrono::Local::now().date_naive(),
            metadata: false,
            roots: false,
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
        };
        Ok(match op {
            Op::Is | Op::IsNot => {
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
            Op::Lt | Op::Le | Op::Gt | Op::Ge => {
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
            Op::Between => {
                let (from, to) = pair(value, number)?;
                let (from, to) = if from <= to { (from, to) } else { (to, from) };
                self.push(from - near(from));
                self.push(to + near(to));
                format!("{column} BETWEEN ? AND ?")
            }
            Op::Empty => format!("{column} IS NULL"),
            Op::NotEmpty => format!("{column} IS NOT NULL"),
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

    fn condition(&mut self, field: Field, op: Op, value: &Value) -> Result<String> {
        match field {
            Field::Text => match op {
                Op::Contains | Op::ContainsAny | Op::NotContains => self.text_any(op, value),
                _ => bail!("Search text supports contains / contains any / does not contain"),
            },
            Field::Filename => {
                // The name after the last `/` of the path in its root.
                let sep = self.sep;
                self.text_column(
                    &format!(
                        "replace(a.rel_path, rtrim(a.rel_path, replace(a.rel_path, '{sep}', '')), '')"
                    ),
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
            Field::Iso => self.number_column("a.iso", op, value, Tolerance::Absolute(0.5)),
            Field::FocalLength => {
                self.number_column("a.focal_length", op, value, Tolerance::Absolute(0.5))
            }
            Field::Aperture => {
                self.number_column("a.aperture", op, value, Tolerance::Absolute(0.05))
            }
            // Cameras round exposure times (1/250 is 0.004 or 1/256).
            Field::Exposure => {
                self.number_column("a.exposure", op, value, Tolerance::Relative(0.03))
            }
            Field::CaptureDate => self.date_column("a.capture_time", op, value),
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
                let folder = PathBuf::from(text_value(value)?);
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
                if parts.is_empty() {
                    "0".into()
                } else {
                    format!("({})", parts.join(" OR "))
                }
            }
            Some((_, rules)) => {
                ensure!(
                    self.expanding.len() < MAX_DEPTH,
                    "Smart albums refer to each other too deeply"
                );
                if !self.expanding.insert(id.to_string()) {
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

    /// Photographs in `folder` (and its subfolders when `below`): those of
    /// the library's own root by their path in it, those of external roots
    /// by the root's folder.
    fn folder(&mut self, folder: &Path, below: bool) -> Result<String> {
        let sep = self.sep;
        let text = |p: &Path| p.to_string_lossy().trim_end_matches(sep).to_string();
        // Library root: the folder relative to the library directory.
        let own = match folder.strip_prefix(self.directory) {
            Ok(relative) => {
                let relative = text(relative);
                let directory_of = format!("rtrim(a.rel_path, replace(a.rel_path, '{sep}', ''))");
                if relative.is_empty() {
                    if below {
                        "a.root_id='session'".to_string()
                    } else {
                        format!("(a.root_id='session' AND {directory_of} = '')")
                    }
                } else if below {
                    self.push(format!("{relative}{sep}"));
                    "(a.root_id='session' AND instr(a.rel_path, ?) = 1)".to_string()
                } else {
                    self.push(format!("{relative}{sep}"));
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
            self.push(format!("{folder}{sep}"));
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
        rule.validate()?;
        let mut compiler = Compiler::new(&self.directory, Some(self));
        let condition = compiler.rule(rule)?;
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
        let compiled = self.compile(rule)?;
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
        let compiled = self.compile(rule)?;
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
        let compiled = self.compile(rule)?;
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
                "SELECT substr(a.capture_time,1,7), a.camera, a.lens, a.kind, a.iso, a.focal_length, a.aperture, {}
                 FROM {} WHERE {}",
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
        Ok(result)
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

    /// Folder and file name rules read the stored paths with the platform's
    /// separator: as a Windows library stores them (`Capture\Day 2\…`).
    #[test]
    fn folders_and_file_names_follow_the_stored_separator() {
        let d = tempfile::tempdir().unwrap();
        let lib = Library::create(&d.path().join("Job"), "Job", Mode::Session).unwrap();
        let capture = lib.directory.join("Capture");
        for name in ["A.ARW", "B.ARW"] {
            std::fs::write(capture.join(name), name).unwrap();
            lib.add_asset(&capture.join(name)).unwrap();
        }
        lib.conn
            .execute_batch(
                "UPDATE assets SET rel_path='Capture\\A.ARW' WHERE rel_path='Capture/A.ARW';
                 UPDATE assets SET rel_path='Capture\\Day 2\\B.ARW' WHERE rel_path='Capture/B.ARW';",
            )
            .unwrap();
        let count = |rule: serde_json::Value| -> i64 {
            let rule: Rule = serde_json::from_value(rule).unwrap();
            let mut compiler = Compiler::new(&lib.directory, Some(&lib));
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
        // On Windows the folder comes as `…\Job\Capture`; here the library
        // directory is joined with the stored relative form.
        let folder = lib.directory.join("Capture");
        let value = folder.to_string_lossy();
        assert_eq!(
            count(serde_json::json!({"field":"folder","op":"is","value":value})),
            1
        );
        assert_eq!(
            count(serde_json::json!({"field":"folder","op":"contains","value":value})),
            2
        );
        let day = format!("{value}\\Day 2");
        assert_eq!(
            count(serde_json::json!({"field":"folder","op":"is","value":day})),
            1
        );
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
