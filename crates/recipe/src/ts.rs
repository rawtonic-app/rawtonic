//! The frontend's recipe type, generated from the key registry (plan 2.3).
//!
//! Each section lists its global keys with their TypeScript type and doc
//! ([`TsSection`]); [`SECTIONS`] holds one entry per section moved into
//! this crate, and [`render`] writes `src/generated/recipeKeys.ts` from
//! them (`cargo run -p rawtonic-recipe --example ts_type`; `scripts/check.sh`
//! fails when the checked-in file differs). Every field is optional, as a
//! saved recipe may lack any key; the frontend's `Adjustments` marks the
//! ones its own objects always hold. Nested value types (`Hsl`,
//! `ColorRange`, …) are named here and stay defined in
//! `src/utils/adjustments.ts`, where the mask layers use them too.

use crate::key::Key;
use std::fmt::Write as _;

/// A key value's TypeScript type.
pub trait TsType {
    /// The type expression.
    const TS: &'static str;
    /// The named type it refers to, imported from `src/utils/adjustments.ts`.
    const IMPORT: Option<&'static str> = None;
}

impl TsType for f64 {
    const TS: &'static str = "number";
}

impl TsType for u64 {
    const TS: &'static str = "number";
}

impl TsType for bool {
    const TS: &'static str = "boolean";
}

impl TsType for String {
    const TS: &'static str = "string";
}

impl TsType for Option<String> {
    const TS: &'static str = "string | null";
}

/// One global key of the TypeScript recipe type.
#[derive(Clone, Copy, Debug)]
pub struct TsField {
    name: &'static str,
    ts: &'static str,
    import: Option<&'static str>,
    doc: &'static str,
}

impl TsField {
    /// `key` with its value's TypeScript type and `doc` (one paragraph,
    /// wrapped when written; empty for none). Takes the key by value: a
    /// reference to a key constant whose default owns memory (a `Vec`)
    /// cannot be taken in a constant.
    pub const fn new<T: TsType>(key: Key<T>, doc: &'static str) -> Self {
        let name = key.into_name();
        Self {
            name,
            ts: T::TS,
            import: T::IMPORT,
            doc,
        }
    }

    pub const fn name(&self) -> &'static str {
        self.name
    }
}

/// A string union the generated file defines (an enum key's values).
#[derive(Clone, Copy, Debug)]
pub struct TsUnion {
    pub name: &'static str,
    pub doc: &'static str,
    pub values: &'static [&'static str],
}

/// A section's part of the generated type.
#[derive(Clone, Copy, Debug)]
pub struct TsSection {
    pub name: &'static str,
    pub fields: &'static [TsField],
    pub unions: &'static [TsUnion],
}

/// Every section in the crate; a new section is one entry here.
pub const SECTIONS: [&TsSection; 6] = [
    &crate::tone::TS,
    &crate::color::TS,
    &crate::detail::TS,
    &crate::geometry::TS,
    &crate::effects::TS,
    &crate::lens::TS,
];

/// Where [`render`]'s text belongs, relative to the repository root.
pub const PATH: &str = "src/generated/recipeKeys.ts";

const WIDTH: usize = 120;

/// The text of `src/generated/recipeKeys.ts`, formatted as Prettier
/// formats it.
pub fn render() -> String {
    let mut out = String::from(
        "// Generated from the recipe crate's key registry (crates/recipe/src/ts.rs) by\n\
         // `cargo run -p rawtonic-recipe --example ts_type`; do not edit. scripts/check.sh\n\
         // fails when this file is stale.\n",
    );
    let mut imports: Vec<&str> = SECTIONS
        .iter()
        .flat_map(|s| s.fields)
        .filter_map(|f| f.import)
        .collect();
    imports.sort_unstable();
    imports.dedup();
    if !imports.is_empty() {
        let line = format!(
            "import type {{ {} }} from '../utils/adjustments';",
            imports.join(", ")
        );
        if line.len() <= WIDTH {
            let _ = writeln!(out, "{line}");
        } else {
            out.push_str("import type {\n");
            for (i, name) in imports.iter().enumerate() {
                let comma = if i + 1 < imports.len() { "," } else { "" };
                let _ = writeln!(out, "  {name}{comma}");
            }
            out.push_str("} from '../utils/adjustments';\n");
        }
    }
    for union in SECTIONS.iter().flat_map(|s| s.unions) {
        out.push('\n');
        doc(&mut out, "", union.doc);
        let values: Vec<String> = union.values.iter().map(|v| format!("'{v}'")).collect();
        let line = format!("export type {} = {};", union.name, values.join(" | "));
        if line.len() <= WIDTH {
            let _ = writeln!(out, "{line}");
        } else {
            let _ = writeln!(
                out,
                "export type {} =\n  {};",
                union.name,
                values.join(" | ")
            );
        }
    }
    out.push_str(
        "\n/** The global recipe keys of the sections in the recipe crate, each optional as in a saved recipe. */\n\
         export interface RecipeKeys {\n",
    );
    for (i, section) in SECTIONS.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let _ = writeln!(out, "  // {}", section.name);
        for field in section.fields {
            doc(&mut out, "  ", field.doc);
            let _ = writeln!(out, "  {}?: {};", field.name, field.ts);
        }
    }
    out.push_str("}\n");
    out
}

/// `text` as a JSDoc comment wrapped to [`WIDTH`]: one line when it fits.
fn doc(out: &mut String, indent: &str, text: &str) {
    if text.is_empty() {
        return;
    }
    let one = format!("{indent}/** {text} */");
    if one.chars().count() <= WIDTH {
        let _ = writeln!(out, "{one}");
        return;
    }
    let _ = writeln!(out, "{indent}/**");
    let prefix = format!("{indent} * ");
    let mut line = prefix.clone();
    for word in text.split_whitespace() {
        if line.len() > prefix.len() && line.chars().count() + 1 + word.chars().count() > WIDTH {
            let _ = writeln!(out, "{line}");
            line = prefix.clone();
        }
        if line.len() > prefix.len() {
            line.push(' ');
        }
        line.push_str(word);
    }
    let _ = writeln!(out, "{line}");
    let _ = writeln!(out, "{indent} */");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names() -> Vec<&'static str> {
        SECTIONS
            .iter()
            .flat_map(|s| s.fields)
            .map(TsField::name)
            .collect()
    }

    #[test]
    fn every_slider_key_is_in_the_type_once() {
        let names = names();
        let sliders = crate::tone::KEYS
            .iter()
            .chain(crate::color::KEYS.iter())
            .chain(crate::detail::KEYS.iter())
            .chain(crate::geometry::KEYS.iter())
            .chain(crate::effects::KEYS.iter());
        for key in sliders {
            assert_eq!(
                names.iter().filter(|n| **n == key.name()).count(),
                1,
                "{}",
                key.name()
            );
        }
        let mut unique = names.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), names.len(), "a key listed twice");
    }

    #[test]
    fn names_are_typescript_identifiers() {
        for name in names() {
            let mut chars = name.chars();
            let first = chars.next().unwrap();
            assert!(
                first.is_alphabetic() && chars.all(char::is_alphanumeric),
                "{name}"
            );
        }
    }

    #[test]
    fn white_balance_union_holds_every_stored_value() {
        use crate::KeyValue;
        use crate::color::WhiteBalance;
        let union = crate::color::TS.unions[0];
        assert_eq!(union.name, WhiteBalance::TS);
        let stored: Vec<String> = WhiteBalance::ALL
            .iter()
            .map(|wb| wb.to_json().as_str().unwrap().to_owned())
            .collect();
        assert_eq!(stored, union.values);
        for wb in WhiteBalance::ALL {
            assert_eq!(WhiteBalance::from_json(&wb.to_json()), Some(wb));
        }
    }

    #[test]
    fn long_docs_wrap_within_the_line_width() {
        let mut out = String::new();
        doc(&mut out, "  ", &"word ".repeat(60));
        assert!(out.lines().count() > 3);
        assert!(out.lines().all(|l| l.chars().count() <= WIDTH));
    }
}
