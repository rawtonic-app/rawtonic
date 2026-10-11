//! Shared, strict filename templates. No filesystem mutation happens here.
use anyhow::{Result, bail, ensure};
use chrono::{Datelike, NaiveDateTime, Timelike};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const TOKENS: &[&str] = &[
    "yyyy",
    "mm",
    "dd",
    "hh",
    "MM",
    "ss",
    "date",
    "time",
    "orig",
    "orig_base",
    "seq",
    "seq_start",
    "job",
    "session",
    "camera",
    "model",
    "serial",
    "lens",
    "iso",
    "rating",
    "color",
    "version",
    "ext",
    "user_text",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Context {
    pub captured: NaiveDateTime,
    pub original: String,
    pub sequence: u64,
    pub sequence_start: u64,
    #[serde(default)]
    pub values: BTreeMap<String, String>,
}

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

    fn value(&self, token: &str) -> Result<String> {
        ensure!(TOKENS.contains(&token), "Unknown naming token: {token}");
        let original = std::path::Path::new(&self.original);
        let dt = self.captured;
        Ok(match token {
            "yyyy" => format!("{:04}", dt.year()),
            "mm" => format!("{:02}", dt.month()),
            "dd" => format!("{:02}", dt.day()),
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
            "orig_base" => original
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .into(),
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
