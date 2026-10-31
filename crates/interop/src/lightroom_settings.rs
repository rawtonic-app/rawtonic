//! Lightroom Classic keeps a photo's develop settings in its catalog as a
//! Lua table (`Adobe_imageDevelopSettings.text`, `s = { Exposure2012 = 0.3,
//! … }`); the XMP packet beside it (`Adobe_AdditionalMetadata.xmp`) holds
//! only the metadata. `to_xmp` writes such a table as the `crs:` XMP
//! Lightroom writes into a sidecar for the same settings, so one converter
//! reads both: scalars as attributes, point curves as `"x, y"` items,
//! lists of strings (brush dabs) as items, lists of tables (masks, spots) as
//! items holding an `rdf:Description`, tables as an `rdf:Description`,
//! booleans as `true`/`false` (as in Lightroom's masks; the converter reads
//! the top-level ones regardless of case).
//!
//! The creative profile (`Look`) is written by name and amount only: its
//! parameters are Adobe's own look data, which Rawtonic neither uses nor
//! keeps.
use std::fmt::Write;

/// A value of the settings table.
#[derive(Debug, Clone, PartialEq)]
pub enum Lua {
    Nil,
    Bool(bool),
    Num(f64),
    Str(String),
    /// Entries in source order; keys of list items are `None`.
    Table(Vec<(Option<String>, Lua)>),
}

impl Lua {
    fn is_list(entries: &[(Option<String>, Lua)]) -> bool {
        !entries.is_empty() && entries.iter().all(|(k, _)| k.is_none())
    }
}

/// Parses `s = { … }` (or a bare `{ … }`).
pub fn parse(text: &str) -> Option<Lua> {
    let mut p = Parser {
        s: text.as_bytes(),
        i: 0,
    };
    p.ws();
    // `name =` before the table.
    let start = p.i;
    if p.ident().is_some() {
        p.ws();
        if p.eat(b'=') {
            p.ws();
        } else {
            p.i = start;
        }
    }
    let value = p.value()?;
    p.ws();
    (p.i == p.s.len() && matches!(value, Lua::Table(_))).then_some(value)
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }
    fn eat(&mut self, c: u8) -> bool {
        if self.peek() == Some(c) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn ws(&mut self) {
        loop {
            while self.peek().is_some_and(|c| c.is_ascii_whitespace()) {
                self.i += 1;
            }
            // `-- comment` to the end of the line.
            if self.s[self.i..].starts_with(b"--") {
                while self.peek().is_some_and(|c| c != b'\n') {
                    self.i += 1;
                }
            } else {
                return;
            }
        }
    }
    fn ident(&mut self) -> Option<String> {
        let start = self.i;
        if !self
            .peek()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == b'_')
        {
            return None;
        }
        while self
            .peek()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == b'_')
        {
            self.i += 1;
        }
        Some(String::from_utf8_lossy(&self.s[start..self.i]).into_owned())
    }
    fn value(&mut self) -> Option<Lua> {
        match self.peek()? {
            b'{' => self.table(),
            b'"' | b'\'' => self.string().map(Lua::Str),
            b'[' => self.long_string().map(Lua::Str),
            c if c == b'-' || c == b'+' || c == b'.' || c.is_ascii_digit() => self.number(),
            _ => match self.ident()?.as_str() {
                "true" => Some(Lua::Bool(true)),
                "false" => Some(Lua::Bool(false)),
                "nil" => Some(Lua::Nil),
                _ => None,
            },
        }
    }
    fn number(&mut self) -> Option<Lua> {
        let start = self.i;
        while self
            .peek()
            .is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'+' | b'.'))
        {
            self.i += 1;
        }
        let text = std::str::from_utf8(&self.s[start..self.i]).ok()?;
        let value = match text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
            Some(hex) => i64::from_str_radix(hex, 16).ok()? as f64,
            None => text.parse::<f64>().ok()?,
        };
        Some(Lua::Num(value))
    }
    fn string(&mut self) -> Option<String> {
        let quote = self.peek()?;
        self.i += 1;
        let mut out = Vec::new();
        loop {
            let c = self.peek()?;
            self.i += 1;
            match c {
                b'\\' => {
                    let e = self.peek()?;
                    self.i += 1;
                    match e {
                        b'n' => out.push(b'\n'),
                        b't' => out.push(b'\t'),
                        b'r' => out.push(b'\r'),
                        b'a' => out.push(7),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'v' => out.push(11),
                        b'\n' => out.push(b'\n'),
                        b'0'..=b'9' => {
                            let mut n = u32::from(e - b'0');
                            for _ in 0..2 {
                                match self.peek() {
                                    Some(d @ b'0'..=b'9') => {
                                        n = n * 10 + u32::from(d - b'0');
                                        self.i += 1;
                                    }
                                    _ => break,
                                }
                            }
                            out.push(u8::try_from(n).ok()?);
                        }
                        other => out.push(other),
                    }
                }
                c if c == quote => break,
                c => out.push(c),
            }
        }
        Some(String::from_utf8_lossy(&out).into_owned())
    }
    /// `[[…]]` or `[==[…]==]`.
    fn long_string(&mut self) -> Option<String> {
        let start = self.i;
        self.i += 1;
        let mut level = 0;
        while self.eat(b'=') {
            level += 1;
        }
        if !self.eat(b'[') {
            self.i = start;
            return None;
        }
        let close: Vec<u8> = std::iter::once(b']')
            .chain(std::iter::repeat_n(b'=', level))
            .chain(std::iter::once(b']'))
            .collect();
        // A newline right after the opening bracket is not part of it.
        if self.peek() == Some(b'\r') {
            self.i += 1;
        }
        if self.peek() == Some(b'\n') {
            self.i += 1;
        }
        let body = self.i;
        let end = self.s[body..]
            .windows(close.len())
            .position(|w| w == close.as_slice())?;
        self.i = body + end + close.len();
        Some(String::from_utf8_lossy(&self.s[body..body + end]).into_owned())
    }
    fn table(&mut self) -> Option<Lua> {
        self.i += 1;
        let mut entries = Vec::new();
        loop {
            self.ws();
            if self.eat(b'}') {
                return Some(Lua::Table(entries));
            }
            let key = if self.peek() == Some(b'[') && self.s.get(self.i + 1) != Some(&b'[') {
                self.i += 1;
                self.ws();
                let key = match self.value()? {
                    Lua::Str(s) => s,
                    Lua::Num(n) => n.to_string(),
                    _ => return None,
                };
                self.ws();
                if !self.eat(b']') {
                    return None;
                }
                self.ws();
                if !self.eat(b'=') {
                    return None;
                }
                Some(key)
            } else {
                let start = self.i;
                match self.ident() {
                    Some(name) if !matches!(name.as_str(), "true" | "false" | "nil") => {
                        self.ws();
                        if self.eat(b'=') {
                            Some(name)
                        } else {
                            self.i = start;
                            None
                        }
                    }
                    _ => {
                        self.i = start;
                        None
                    }
                }
            };
            self.ws();
            let value = self.value()?;
            entries.push((key, value));
            self.ws();
            if !self.eat(b',') && !self.eat(b';') {
                self.ws();
                if !self.eat(b'}') {
                    return None;
                }
                return Some(Lua::Table(entries));
            }
        }
    }
}

const CRS: &str = "http://ns.adobe.com/camera-raw-settings/1.0/";
const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";

/// The `crs:` XMP packet for Lightroom catalog settings `text`, or `None`
/// when `text` is not a settings table.
pub fn to_xmp(text: &str) -> Option<String> {
    let Lua::Table(entries) = parse(text)? else {
        return None;
    };
    if entries.is_empty() || Lua::is_list(&entries) {
        return None;
    }
    let mut out = String::from("<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"");
    out.push_str(RDF);
    out.push_str("\"><rdf:Description rdf:about=\"\" xmlns:crs=\"");
    out.push_str(CRS);
    out.push('"');
    let entries: Vec<(Option<String>, Lua)> = entries
        .into_iter()
        .map(|(key, value)| match (key.as_deref(), value) {
            (Some("Look"), Lua::Table(look)) => (key, look_by_name(look)),
            (_, value) => (key, value),
        })
        .collect();
    description_body(&mut out, &entries);
    if !out.ends_with("/>") {
        out.push_str("</rdf:Description>");
    }
    out.push_str("</rdf:RDF></x:xmpmeta>");
    Some(out)
}

/// The creative profile without Adobe's look data: name and amount.
fn look_by_name(look: Vec<(Option<String>, Lua)>) -> Lua {
    Lua::Table(
        look.into_iter()
            .filter(|(k, v)| {
                matches!(k.as_deref(), Some("Name" | "Amount" | "UUID"))
                    && matches!(v, Lua::Str(_) | Lua::Num(_))
            })
            .collect(),
    )
}

/// Attributes (scalars) close the start tag `out` holds open; nested values
/// follow as elements.
fn description_body(out: &mut String, entries: &[(Option<String>, Lua)]) {
    let mut children = String::new();
    for (key, value) in entries {
        let Some(key) = key.as_deref().filter(|k| valid_name(k)) else {
            continue;
        };
        match value {
            Lua::Nil => {}
            Lua::Bool(b) => {
                let _ = write!(out, " crs:{key}=\"{}\"", if *b { "true" } else { "false" });
            }
            Lua::Num(n) => {
                let _ = write!(out, " crs:{key}=\"{}\"", number(*n));
            }
            Lua::Str(s) => {
                let _ = write!(out, " crs:{key}=\"{}\"", escape(s));
            }
            Lua::Table(items) => element(&mut children, key, items),
        }
    }
    if children.is_empty() {
        out.push_str("/>");
    } else {
        out.push('>');
        out.push_str(&children);
    }
}

fn element(out: &mut String, key: &str, items: &[(Option<String>, Lua)]) {
    if items.is_empty() {
        return;
    }
    if !Lua::is_list(items) {
        let _ = write!(out, "<crs:{key}><rdf:Description");
        description_body(out, items);
        if out.ends_with("/>") {
            let _ = write!(out, "</crs:{key}>");
        } else {
            let _ = write!(out, "</rdf:Description></crs:{key}>");
        }
        return;
    }
    let values: Vec<&Lua> = items.iter().map(|(_, v)| v).collect();
    let _ = write!(out, "<crs:{key}><rdf:Seq>");
    if values.iter().all(|v| matches!(v, Lua::Num(_))) {
        // Point curves: x, y pairs.
        for pair in values.chunks(2) {
            let n = |v: &Lua| match v {
                Lua::Num(n) => number(*n),
                _ => String::new(),
            };
            match pair {
                [x, y] => {
                    let _ = write!(out, "<rdf:li>{}, {}</rdf:li>", n(x), n(y));
                }
                [x] => {
                    let _ = write!(out, "<rdf:li>{}</rdf:li>", n(x));
                }
                _ => {}
            }
        }
    } else {
        for value in values {
            match value {
                Lua::Str(s) => {
                    let _ = write!(out, "<rdf:li>{}</rdf:li>", escape(s));
                }
                Lua::Num(n) => {
                    let _ = write!(out, "<rdf:li>{}</rdf:li>", number(*n));
                }
                Lua::Bool(b) => {
                    let _ = write!(
                        out,
                        "<rdf:li>{}</rdf:li>",
                        if *b { "true" } else { "false" }
                    );
                }
                Lua::Table(fields) if !Lua::is_list(fields) => {
                    out.push_str("<rdf:li><rdf:Description");
                    description_body(out, fields);
                    if out.ends_with("/>") {
                        out.push_str("</rdf:li>");
                    } else {
                        out.push_str("</rdf:Description></rdf:li>");
                    }
                }
                Lua::Table(_) | Lua::Nil => {}
            }
        }
    }
    let _ = write!(out, "</rdf:Seq></crs:{key}>");
}

fn valid_name(key: &str) -> bool {
    key.bytes()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == b'_')
        && key.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
}

fn number(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\n', "&#xA;")
}

#[cfg(test)]
mod tests {
    use super::*;

    // Written for this test in the shape Lightroom Classic 15 stores
    // (not copied from Lightroom's data).
    const SAMPLE: &str = r#"s = { AILook = {  },
ConvertToGrayscale = true,
Exposure2012 = 0.3,
HasCrop = true,
Look = { Amount = 1,
Name = "Some Look",
Parameters = { ConvertToGrayscale = false,
ToneCurvePV2012 = { 0, 0, 22, 16, 255, 255 } } },
MaskGroupBasedCorrections = { { CorrectionMasks = { { MaskName = "Brush \"1\"",
Masks = { { Dabs = { "d 0.1 0.2",
"d 0.3 0.4" },
Radius = 0.1,
What = "Mask/Paint" } },
What = "Mask/Aggregate" } },
LocalExposure2012 = -0.25,
What = "Correction" } },
RetouchAreas = { { Masks = { { What = "Mask/Ellipse", X = 0.45, Y = 0.13 } },
SpotType = "heal" } },
ToneCurveName2012 = "Custom",
ToneCurvePV2012 = { 0,
0,
64,
56,
255,
255 },
WhiteBalance = "As Shot",
["x-default"] = "skipped" }"#;

    #[test]
    fn parses_lightrooms_settings_table() {
        let Some(Lua::Table(entries)) = parse(SAMPLE) else {
            panic!("not parsed");
        };
        let get = |k: &str| entries.iter().find(|(key, _)| key.as_deref() == Some(k));
        assert_eq!(get("Exposure2012").unwrap().1, Lua::Num(0.3));
        assert_eq!(get("WhiteBalance").unwrap().1, Lua::Str("As Shot".into()));
        assert_eq!(get("x-default").unwrap().1, Lua::Str("skipped".into()));
        assert!(parse("s = { a = 1").is_none());
        assert!(parse("garbage").is_none());
        assert_eq!(
            parse(r#"{ "a\"b\n", [[long]], 0x10, -1e-3 }"#),
            Some(Lua::Table(vec![
                (None, Lua::Str("a\"b\n".into())),
                (None, Lua::Str("long".into())),
                (None, Lua::Num(16.0)),
                (None, Lua::Num(-0.001)),
            ]))
        );
    }

    #[test]
    fn writes_the_sidecar_shape() {
        let xmp = to_xmp(SAMPLE).unwrap();
        roxmltree::Document::parse(&xmp).expect("well-formed");
        assert!(xmp.contains(r#"crs:Exposure2012="0.3""#));
        assert!(xmp.contains(r#"crs:ConvertToGrayscale="true""#));
        assert!(xmp.contains(r#"crs:HasCrop="true""#));
        assert!(xmp.contains("<crs:ToneCurvePV2012><rdf:Seq><rdf:li>0, 0</rdf:li><rdf:li>64, 56</rdf:li><rdf:li>255, 255</rdf:li></rdf:Seq></crs:ToneCurvePV2012>"));
        assert!(xmp.contains(
            r#"<crs:Look><rdf:Description crs:Amount="1" crs:Name="Some Look"/></crs:Look>"#
        ));
        // Adobe's look data is not carried over (and cannot override the
        // photo's own settings).
        assert!(!xmp.contains("Parameters"));
        assert_eq!(xmp.matches("ConvertToGrayscale").count(), 1);
        assert!(xmp.contains(r#"crs:MaskName="Brush &quot;1&quot;""#));
        assert!(xmp.contains("<crs:Dabs><rdf:Seq><rdf:li>d 0.1 0.2</rdf:li><rdf:li>d 0.3 0.4</rdf:li></rdf:Seq></crs:Dabs>"));
        assert!(xmp.contains(r#"<rdf:li><rdf:Description crs:What="Mask/Ellipse" crs:X="0.45" crs:Y="0.13"/></rdf:li>"#));
        assert!(!xmp.contains("x-default") && !xmp.contains("AILook"));
        assert!(to_xmp("s = { 1, 2 }").is_none());
        // Scalars only: the description closes itself.
        let scalars = to_xmp("s = { Exposure2012 = 0.3, ConvertToGrayscale = true }").unwrap();
        roxmltree::Document::parse(&scalars).expect("well-formed");
    }
}
