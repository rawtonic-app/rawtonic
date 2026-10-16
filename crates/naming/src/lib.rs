//! Shared, strict filename templates. No filesystem mutation happens here.
use anyhow::{Result, bail, ensure};
use chrono::{Datelike, NaiveDateTime, Timelike};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Every token a template may use. Backwards compatible: tokens are only
/// ever added, never renamed (`mm` is the month and `MM` the minute, as
/// since the first version). A token whose value is unknown for a photo
/// (no lens in its EXIF, say) renders empty.
pub const TOKENS: &[&str] = &[
    // Capture date and time (`Capture::captured`, else the file date).
    "yyyy",
    "yy",
    "mm",
    "month",
    "mon",
    "dd",
    "doy",
    "weekday",
    "hh",
    "MM",
    "ss",
    "subsec",
    "tz",
    "date",
    "time",
    // The original file.
    "orig",
    "orig_base",
    "orig_num",
    "folder",
    "ext",
    // Counter.
    "seq",
    "seq_start",
    // Library and free text.
    "job",
    "session",
    "version",
    "user_text",
    // Camera and exposure (EXIF).
    "make",
    "model",
    "camera",
    "serial",
    "lens",
    "iso",
    "focal",
    "aperture",
    "shutter",
    // Descriptive metadata (the card's XMP sidecar, else EXIF).
    "rating",
    "color",
    "title",
    "creator",
    "copyright",
];

/// Where a capture date came from, best first. Ingest tries them in this
/// order: EXIF DateTimeOriginal (the shutter release; for maker formats
/// such as RAF, CR3 or CRW as the RAW decoder reads their embedded EXIF),
/// EXIF DateTimeDigitized, the EXIF file date (IFD0 DateTime, which a
/// camera writes when it saves the shot), then the file's modification
/// time, and only when even that is unreadable the time of the import.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DateSource {
    Original,
    Digitized,
    FileDate,
    Modified,
    #[default]
    Now,
}

/// What a photograph's own metadata says about it, for the naming tokens.
/// Read by `rawtonic_ingest::capture` from the file's header (and its card
/// sidecar); every field is optional and a missing one renders empty.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Capture {
    /// Local capture time as the camera recorded it.
    pub captured: Option<NaiveDateTime>,
    pub date_source: DateSource,
    /// Fraction of a second as written (EXIF SubSecTimeOriginal digits).
    pub subsec: Option<String>,
    /// UTC offset of `captured` (EXIF OffsetTimeOriginal), `+02:00`.
    pub offset: Option<String>,
    pub make: Option<String>,
    pub model: Option<String>,
    pub serial: Option<String>,
    pub lens: Option<String>,
    pub iso: Option<u32>,
    /// Millimetres.
    pub focal_length: Option<f64>,
    /// F-number.
    pub aperture: Option<f64>,
    /// Exposure time in seconds as a fraction (numerator, denominator).
    pub exposure: Option<(u32, u32)>,
    pub creator: Option<String>,
    pub copyright: Option<String>,
    pub title: Option<String>,
    pub rating: Option<u8>,
    /// Colour label (`Red`, …).
    pub label: Option<String>,
}

fn trim_number(value: f64) -> String {
    let text = format!("{value:.1}");
    text.strip_suffix(".0").unwrap_or(&text).to_string()
}

/// `1-250` for 1/250 s (a filename cannot hold `/`), else seconds with a
/// unit: `0.3s`, `2s`, `2.5s`.
pub fn shutter_text(numerator: u32, denominator: u32) -> Option<String> {
    if numerator == 0 || denominator == 0 {
        return None;
    }
    let seconds = numerator as f64 / denominator as f64;
    let inverse = 1.0 / seconds;
    if seconds <= 0.5 && (inverse - inverse.round()).abs() <= 0.05 * inverse.round() {
        return Some(format!("1-{}", inverse.round() as u64));
    }
    Some(format!("{}s", trim_number(seconds)))
}

fn text(value: &Option<String>) -> Option<String> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(String::from)
}

impl Capture {
    /// `make` and `model` as one name, the make left out when the model
    /// already starts with it (`Canon EOS R5`, `Sony ILCE-7M4`).
    pub fn camera(&self) -> Option<String> {
        match (text(&self.make), text(&self.model)) {
            (Some(make), Some(model)) if model.to_lowercase().starts_with(&make.to_lowercase()) => {
                Some(model)
            }
            (Some(make), Some(model)) => Some(format!("{make} {model}")),
            (None, Some(model)) => Some(model),
            (Some(make), None) => Some(make),
            (None, None) => None,
        }
    }

    /// The token values this capture fills (the date tokens come from
    /// `captured`).
    fn values(&self) -> Vec<(&'static str, String)> {
        [
            ("subsec", text(&self.subsec)),
            ("tz", text(&self.offset).map(|o| o.replace(':', ""))),
            ("make", text(&self.make)),
            ("model", text(&self.model)),
            ("camera", self.camera()),
            ("serial", text(&self.serial)),
            ("lens", text(&self.lens)),
            ("iso", self.iso.filter(|i| *i > 0).map(|i| i.to_string())),
            (
                "focal",
                self.focal_length.filter(|f| *f > 0.0).map(trim_number),
            ),
            (
                "aperture",
                self.aperture.filter(|f| *f > 0.0).map(trim_number),
            ),
            (
                "shutter",
                self.exposure.and_then(|(n, d)| shutter_text(n, d)),
            ),
            ("creator", text(&self.creator)),
            ("copyright", text(&self.copyright)),
            ("title", text(&self.title)),
            ("rating", self.rating.map(|r| r.to_string())),
            ("color", text(&self.label)),
        ]
        .into_iter()
        .filter_map(|(k, v)| v.map(|v| (k, v)))
        .collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Context {
    pub captured: NaiveDateTime,
    pub original: String,
    pub sequence: u64,
    pub sequence_start: u64,
    #[serde(default)]
    pub values: BTreeMap<String, String>,
}

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

impl Context {
    pub fn new(original: impl Into<String>, sequence: u64) -> Self {
        Self {
            captured: chrono::Local::now().naive_local(),
            original: original.into(),
            sequence,
            sequence_start: 1,
            values: BTreeMap::new(),
        }
    }

    /// Fills the date and metadata tokens from `capture`; values the
    /// caller set for other tokens (`job`, `folder`, …) stay. The date
    /// stays as it is when the capture has none.
    pub fn apply_capture(&mut self, capture: &Capture) {
        if let Some(captured) = capture.captured {
            self.captured = captured;
        }
        for (token, value) in capture.values() {
            self.values.insert(token.into(), value);
        }
    }

    fn value(&self, token: &str) -> Result<String> {
        ensure!(TOKENS.contains(&token), "Unknown naming token: {token}");
        let original = std::path::Path::new(&self.original);
        let dt = self.captured;
        let stem = || {
            original
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string()
        };
        Ok(match token {
            "yyyy" => format!("{:04}", dt.year()),
            "yy" => format!("{:02}", dt.year().rem_euclid(100)),
            "mm" => format!("{:02}", dt.month()),
            "month" => MONTHS[dt.month0() as usize].into(),
            "mon" => MONTHS[dt.month0() as usize][..3].into(),
            "dd" => format!("{:02}", dt.day()),
            "doy" => format!("{:03}", dt.ordinal()),
            "weekday" => dt.format("%A").to_string(),
            "hh" => format!("{:02}", dt.hour()),
            "MM" => format!("{:02}", dt.minute()),
            "ss" => format!("{:02}", dt.second()),
            "date" => dt.format("%Y-%m-%d").to_string(),
            "time" => dt.format("%H-%M-%S").to_string(),
            "orig" => original
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .into(),
            "orig_base" => stem(),
            // The camera's file number: the digits its name ends with
            // (`DSC_01234` → `01234`).
            "orig_num" => {
                let stem = stem();
                let digits = stem.chars().rev().take_while(char::is_ascii_digit).count();
                stem[stem.len() - digits..].to_string()
            }
            "ext" => original
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .into(),
            "seq" => self.sequence.to_string(),
            "seq_start" => self.sequence_start.to_string(),
            _ => self.values.get(token).cloned().unwrap_or_default(),
        })
    }
}

fn width(value: &str) -> Result<usize> {
    let width: usize = value.parse()?;
    ensure!(
        (1..=20).contains(&width),
        "Padding must be between 1 and 20"
    );
    Ok(width)
}

fn expand(expression: &str, context: &Context) -> Result<String> {
    let mut parts = expression.split('|');
    let token = parts.next().unwrap_or_default();
    let (name, pad) = token
        .split_once(':')
        .map_or((token, None), |(a, b)| (a, Some(b)));
    let mut value = context.value(name)?;
    if let Some(pad) = pad {
        ensure!(name == "seq", "Use |pad:N for non-sequence tokens");
        value = format!("{value:0>width$}", width = width(pad)?);
    }
    for transform in parts {
        value = match transform {
            "upper" => value.to_uppercase(),
            "lower" => value.to_lowercase(),
            x if x.starts_with("pad:") => format!("{value:0>width$}", width = width(&x[4..])?),
            _ => bail!("Unknown naming transform: {transform}"),
        };
    }
    // A token can never inject a folder, even in render_relative().
    Ok(value
        .chars()
        .map(|c| if invalid(c) { '_' } else { c })
        .collect())
}

fn invalid(c: char) -> bool {
    c.is_control() || "/\\:*?\"<>|".contains(c)
}

pub fn validate_component(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty() && name.len() <= 240,
        "Filename must contain 1–240 bytes"
    );
    ensure!(
        name != "." && name != "..",
        "Relative traversal is not a filename"
    );
    ensure!(
        !name.ends_with(['.', ' ']),
        "Filename cannot end in a dot or space"
    );
    ensure!(
        !name.chars().any(invalid),
        "Filename contains a reserved character"
    );
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    ensure!(
        !reserved.contains(&stem.as_str()),
        "Reserved device filename: {name}"
    );
    Ok(())
}

/// Render a filename, rejecting unknown tokens and malformed templates.
pub fn render(template: &str, context: &Context) -> Result<String> {
    ensure!(template.len() <= 4096, "Template too long");
    let mut output = String::new();
    let mut remaining = template;
    while let Some(start) = remaining.find('{') {
        let prefix = &remaining[..start];
        ensure!(!prefix.contains('}'), "Unmatched closing brace");
        output.push_str(prefix);
        let rest = &remaining[start + 1..];
        let end = rest
            .find('}')
            .ok_or_else(|| anyhow::anyhow!("Unclosed token"))?;
        output.push_str(&expand(&rest[..end], context)?);
        remaining = &rest[end + 1..];
    }
    ensure!(!remaining.contains('}'), "Unmatched closing brace");
    output.push_str(remaining);
    validate_component(&output)?;
    Ok(output)
}

/// Folder separators must be literal `/` in the template. No absolute paths.
pub fn render_relative(template: &str, context: &Context) -> Result<std::path::PathBuf> {
    let parts = template
        .split('/')
        .map(|part| render(part, context))
        .collect::<Result<Vec<_>>>()?;
    Ok(parts.iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn every_date_token_and_transform() {
        let mut c = Context::new("DSC01234.ARW", 12);
        c.captured =
            NaiveDateTime::parse_from_str("2026-10-01 14:37:08", "%Y-%m-%d %H:%M:%S").unwrap();
        c.values.insert("job".into(), "Acme / Zürich".into());
        assert_eq!(
            render(
                "{yyyy}{mm}{dd}_{hh}{MM}{ss}_{seq:4}_{orig_base|lower}.{ext|lower}",
                &c
            )
            .unwrap(),
            "20261001_143708_0012_dsc01234.arw"
        );
        assert_eq!(render("{job|upper}", &c).unwrap(), "ACME _ ZÜRICH");
        assert_eq!(
            render_relative("{yyyy}/{mm}/{orig}", &c).unwrap(),
            std::path::PathBuf::from("2026/10/DSC01234.ARW")
        );
    }

    #[test]
    fn fails_closed_on_invalid_names() {
        let c = Context::new("a.arw", 1);
        for template in [
            "{oops}",
            "{seq:9999}",
            "{orig",
            "a}",
            "../{orig}",
            "CON",
            "a.",
            "{orig|oops}",
        ] {
            assert!(render(template, &c).is_err(), "{template}");
        }
        assert!(render_relative("../{orig}", &c).is_err());
        assert!(render_relative("/{orig}", &c).is_err());
    }

    fn capture() -> Capture {
        Capture {
            captured: Some(
                NaiveDateTime::parse_from_str("2026-03-07 09:05:02", "%Y-%m-%d %H:%M:%S").unwrap(),
            ),
            date_source: DateSource::Original,
            subsec: Some("042".into()),
            offset: Some("+02:00".into()),
            make: Some("Canon".into()),
            model: Some("Canon EOS R5".into()),
            serial: Some("012345678901".into()),
            lens: Some("RF24-70mm F2.8 L IS USM".into()),
            iso: Some(800),
            focal_length: Some(35.0),
            aperture: Some(2.8),
            exposure: Some((1, 250)),
            creator: Some("A. Photographer".into()),
            copyright: Some("© 2026 A. Photographer".into()),
            title: Some("Harbour".into()),
            rating: Some(4),
            label: Some("Red".into()),
        }
    }

    #[test]
    fn capture_tokens_render_from_exif_facts() {
        let mut c = Context::new("IMG_0042.CR3", 7);
        c.values.insert("folder".into(), "100CANON".into());
        c.apply_capture(&capture());
        let render = |t: &str| render(t, &c).unwrap();
        assert_eq!(
            render("{date}_{time}.{subsec}{tz}"),
            "2026-03-07_09-05-02.042+0200"
        );
        assert_eq!(render("{yy}{mm}{dd}-{doy}"), "260307-066");
        assert_eq!(render("{month} {mon} {weekday}"), "March Mar Saturday");
        assert_eq!(render("{camera}"), "Canon EOS R5");
        assert_eq!(
            render("{make}-{model}-{serial}"),
            "Canon-Canon EOS R5-012345678901"
        );
        assert_eq!(render("{lens}"), "RF24-70mm F2.8 L IS USM");
        assert_eq!(
            render("ISO{iso}_{focal}mm_f{aperture}_{shutter}"),
            "ISO800_35mm_f2.8_1-250"
        );
        assert_eq!(render("{orig_num}_{folder}"), "0042_100CANON");
        assert_eq!(render("{rating}{color|lower}_{title}"), "4red_Harbour");
        assert_eq!(render("{creator}"), "A. Photographer");
        // `©` is a valid filename character.
        assert_eq!(render("{copyright}"), "© 2026 A. Photographer");
    }

    #[test]
    fn make_is_not_repeated_in_the_camera_name() {
        let mut c = Capture {
            make: Some("Sony".into()),
            model: Some("ILCE-7M4".into()),
            ..Capture::default()
        };
        assert_eq!(c.camera().as_deref(), Some("Sony ILCE-7M4"));
        c.model = Some("SONY ILCE-7M4".into());
        assert_eq!(c.camera().as_deref(), Some("SONY ILCE-7M4"));
        c.make = None;
        assert_eq!(c.camera().as_deref(), Some("SONY ILCE-7M4"));
        assert_eq!(Capture::default().camera(), None);
    }

    #[test]
    fn shutter_speeds_are_filename_safe() {
        assert_eq!(shutter_text(1, 250).as_deref(), Some("1-250"));
        assert_eq!(shutter_text(10, 2500).as_deref(), Some("1-250"));
        assert_eq!(shutter_text(1, 3).as_deref(), Some("1-3"));
        assert_eq!(shutter_text(1, 2).as_deref(), Some("1-2"));
        assert_eq!(shutter_text(3, 10).as_deref(), Some("0.3s"));
        assert_eq!(shutter_text(2, 1).as_deref(), Some("2s"));
        assert_eq!(shutter_text(25, 10).as_deref(), Some("2.5s"));
        assert_eq!(shutter_text(0, 1), None);
        assert_eq!(shutter_text(1, 0), None);
    }

    /// Templates written before EXIF hydration keep their meaning, and a
    /// token without a value renders empty instead of failing the import.
    #[test]
    fn older_templates_and_missing_values_keep_working() {
        let mut c = Context::new("DSC_0001.ARW", 3);
        c.captured =
            NaiveDateTime::parse_from_str("2026-10-01 14:37:08", "%Y-%m-%d %H:%M:%S").unwrap();
        c.apply_capture(&Capture::default());
        assert_eq!(
            render("{yyyy}{mm}{dd}_{hh}{MM}{ss}_{seq:4}_{orig}", &c).unwrap(),
            "20261001_143708_0003_DSC_0001.ARW"
        );
        assert_eq!(
            render("{lens}{orig_base}_{camera}x", &c).unwrap(),
            "DSC_0001_x"
        );
        assert_eq!(
            render("{orig_num}", &Context::new("photo.jpg", 1)).ok(),
            None
        );
    }

    proptest! {
        #[test]
        fn tokens_cannot_inject_paths(value in ".{0,150}") {
            let mut c = Context::new("a.arw", 1);
            c.values.insert("job".into(), value);
            if let Ok(result) = render("{job}.arw", &c) {
                prop_assert!(!result.contains('/'));
                prop_assert!(!result.contains('\\'));
                prop_assert_eq!(std::path::Path::new(&result).components().count(), 1);
            }
        }
    }
}
