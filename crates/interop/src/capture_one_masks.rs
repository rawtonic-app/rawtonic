//! Capture One's layer masks (`.comask`, one file per photograph; notes:
//! benchmark-results/c1l1-2026-11-17/FORMAT.md has the observations this
//! reader rests on).
//!
//! The file is a tagged binary container. A field is a length-prefixed
//! ASCII key, a zero byte, a type byte (`0x22`: bytes, `0x00`: a map
//! block), `0xff` padding and the value's length as a variable-length
//! number: one byte below 64, or a first byte `0x40 | n` followed by base-64
//! digits, the last of them marked with `0x40`. `MaskInfo`/`Hdr` holds the
//! frame (width, height) the masks are drawn in; each mask then follows as
//! `MaskIndex`, `UserInfo`/`Info` (the layer's mask UUID, as in the
//! `MaskUUID` of its layer), `MaskType`, `CompParam` and its shape:
//!
//! - `LinearGradient`: end x, end y, start x, start y (pixels of the frame),
//!   and 0.5; full effect beyond the start line, none beyond the end line,
//!   a smoothstep between them.
//! - `RadialGradient`: centre x, y, rotation, vertical and horizontal
//!   radius, 0, the inner share of the radii (0.5: full effect inside half
//!   the ellipse, a smoothstep to none on it), and 1 when the effect lies
//!   outside the ellipse (as drawn) or 0 inside (inverted).
//! - `CompositeMask` n: the next n masks combine; a Luma Range is the
//!   layer's raster (`Map`) followed by a `LumaRange` (low, high, falloff
//!   low end, falloff high end on 0–255, radius, sensitivity, invert).
//! - `Map`: a raster of 64 × 64 px tiles, one code per tile: `00 v` a
//!   uniform tile of value v, `01` another tile of the last uniform value,
//!   `08`/`09 hi lo` a compressed tile. Compressed tiles (brushes, colour
//!   ranges, AI masks) are not decoded; only uniform rasters (filled or
//!   cleared masks) are read.
use std::path::{Path, PathBuf};

/// A layer mask as far as Rawtonic reads it.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum Mask {
    /// The same value (0–255) everywhere: a filled or cleared mask.
    Uniform { value: u8 },
    /// Full effect at `start`, none at `end` (frame pixels).
    Linear {
        start: (f64, f64),
        end: (f64, f64),
        midpoint: f64,
    },
    Radial {
        center: (f64, f64),
        rotation: f64,
        radius_x: f64,
        radius_y: f64,
        /// The unnamed sixth number (0 in every mask seen).
        reserved: f64,
        /// Share of the radii with full effect (0.5 in every mask seen).
        inner: f64,
        /// The effect lies outside the ellipse (Capture One's default).
        outside: bool,
    },
    /// A raster of one value limited to a Luma Range (0–255 scale).
    LumaRange {
        base: u8,
        low: f64,
        high: f64,
        falloff_low: f64,
        falloff_high: f64,
        radius: f64,
        sensitivity: f64,
        invert: bool,
    },
    /// A painted, colour-range or AI mask (compressed raster tiles), or one
    /// in a form not read (named for the import report).
    Unsupported { what: String },
}

/// The masks of one `.comask` file.
#[derive(Debug, Clone, Default)]
pub struct MaskFile {
    /// Width and height of the frame the masks are drawn in.
    pub frame: (f64, f64),
    records: Vec<Record>,
}

#[derive(Debug, Clone, Default)]
struct Record {
    uuid: String,
    name: String,
    mask_type: Option<[u8; 4]>,
    composite: Option<u32>,
    linear: Option<Vec<f32>>,
    radial: Option<(Vec<f32>, i32)>,
    luma: Option<(Vec<f32>, i32)>,
    /// `Some(Some(v))`: a uniform raster of `v`; `Some(None)`: compressed
    /// tiles.
    map: Option<Option<u8>>,
}

/// One field: key, type byte and value bytes.
struct Field<'a> {
    key: &'a str,
    value: &'a [u8],
}

fn varint(d: &[u8], mut i: usize) -> Option<(usize, usize)> {
    let b = *d.get(i)?;
    if b < 0x40 {
        return Some((b as usize, i + 1));
    }
    let mut v = (b & 0x3f) as usize;
    i += 1;
    for _ in 0..8 {
        let b = *d.get(i)?;
        i += 1;
        if b & 0x40 != 0 {
            return Some(((v << 6) | (b & 0x3f) as usize, i));
        }
        v = (v << 6) | b as usize;
    }
    None
}

/// The field starting at `i`, if one does, and where the next byte is.
fn field_at(d: &[u8], i: usize) -> Option<(Field<'_>, usize)> {
    let len = *d.get(i)? as usize;
    if !(3..=24).contains(&len) {
        return None;
    }
    let key = d.get(i + 1..i + 1 + len)?;
    if !key.iter().all(u8::is_ascii_alphabetic) || !key[0].is_ascii_uppercase() {
        return None;
    }
    let mut j = i + 1 + len;
    if *d.get(j)? != 0 {
        return None;
    }
    let kind = *d.get(j + 1)?;
    if kind != 0x22 && kind != 0x00 {
        return None;
    }
    j += 2;
    let key = std::str::from_utf8(key).ok()?;
    // Containers (`MaskInfo`, `UserInfo`) hold fields; a map block, a
    // value. Padding precedes a length.
    if kind == 0x22 && *d.get(j)? == 0x00 {
        return Some((Field { key, value: &[] }, j));
    }
    while *d.get(j)? == 0xff {
        j += 1;
    }
    let (n, j) = varint(d, j)?;
    let value = d.get(j..j + n)?;
    Some((Field { key, value }, j + n))
}

fn floats(v: &[u8], n: usize) -> Option<Vec<f32>> {
    (v.len() >= 4 * n).then(|| {
        (0..n)
            .map(|k| f32::from_le_bytes(v[4 * k..4 * k + 4].try_into().unwrap()))
            .collect()
    })
}

fn int_at(v: &[u8], k: usize) -> Option<i32> {
    Some(i32::from_le_bytes(
        v.get(4 * k..4 * k + 4)?.try_into().ok()?,
    ))
}

/// The value of a raster whose tiles are all uniform, else None.
fn uniform_map(m: &[u8]) -> Option<u8> {
    let (mut i, mut current, mut seen) = (0, 0u8, None::<u8>);
    while i < m.len() {
        match m[i] {
            0x00 => {
                current = *m.get(i + 1)?;
                i += 2;
            }
            0x01 => i += 1,
            _ => return None,
        }
        if seen.is_some_and(|s| s != current) {
            return None;
        }
        seen = Some(current);
    }
    seen
}

impl MaskFile {
    pub fn parse(d: &[u8]) -> Option<Self> {
        if d.get(1..5) != Some(b"XCon") {
            return None;
        }
        let mut file = MaskFile::default();
        let mut i = 5;
        let mut current: Option<Record> = None;
        while i < d.len() {
            let Some((f, next)) = field_at(d, i) else {
                i += 1;
                continue;
            };
            match f.key {
                // The compressed tiles follow the last mask.
                "Blk" => break,
                "Hdr" if file.frame == (0.0, 0.0) => {
                    if let (Some(w), Some(h)) = (int_at(f.value, 1), int_at(f.value, 2)) {
                        file.frame = (w as f64, h as f64);
                    }
                }
                "MaskIndex" => {
                    file.records.extend(current.take());
                    current = Some(Record::default());
                }
                _ => {
                    if let Some(r) = current.as_mut() {
                        match f.key {
                            "Info" => r.uuid = String::from_utf8_lossy(f.value).into_owned(),
                            "Name" => r.name = String::from_utf8_lossy(f.value).into_owned(),
                            "MaskType" => r.mask_type = f.value.get(..4)?.try_into().ok(),
                            "CompositeMask" => r.composite = int_at(f.value, 0).map(|n| n as u32),
                            "LinearGradient" => r.linear = floats(f.value, 5),
                            "RadialGradient" => {
                                r.radial = floats(f.value, 7).zip(int_at(f.value, 7));
                            }
                            "LumaRange" => r.luma = floats(f.value, 6).zip(int_at(f.value, 6)),
                            "Map" => r.map = Some(uniform_map(f.value)),
                            _ => {}
                        }
                    }
                }
            }
            i = next;
        }
        file.records.extend(current);
        (file.frame.0 > 0.0 && file.frame.1 > 0.0).then_some(file)
    }

    /// Whether the file holds the mask `uuid`.
    pub fn has(&self, uuid: &str) -> bool {
        self.records
            .iter()
            .any(|r| r.uuid.eq_ignore_ascii_case(uuid))
    }

    /// The mask of the layer whose `MaskUUID` is `uuid`.
    pub fn mask(&self, uuid: &str) -> Mask {
        let unsupported = |what: &str| Mask::Unsupported { what: what.into() };
        // The layer's own record comes first; a composite's parts follow it.
        let Some(at) = self
            .records
            .iter()
            .position(|r| r.uuid.eq_ignore_ascii_case(uuid))
        else {
            return unsupported("missing mask");
        };
        let r = &self.records[at];
        if r.name.contains("MaskGenerator") {
            return unsupported("AI mask");
        }
        if let Some(n) = r.composite {
            let parts = self.records.get(at + 1..at + 1 + n as usize);
            return match parts {
                Some([base, range]) => match (base.map, &range.luma) {
                    (Some(Some(value)), Some((v, invert))) if v.len() == 6 => Mask::LumaRange {
                        base: value,
                        low: v[0] as f64,
                        high: v[1] as f64,
                        falloff_low: v[2] as f64,
                        falloff_high: v[3] as f64,
                        radius: v[4] as f64,
                        sensitivity: v[5] as f64,
                        invert: *invert != 0,
                    },
                    (Some(None), Some(_)) => unsupported("painted mask with a luma range"),
                    _ => unsupported("combined mask"),
                },
                _ => unsupported("combined mask"),
            };
        }
        if let Some(v) = &r.linear {
            return Mask::Linear {
                start: (v[2] as f64, v[3] as f64),
                end: (v[0] as f64, v[1] as f64),
                midpoint: v[4] as f64,
            };
        }
        if let Some((v, outside)) = &r.radial {
            return Mask::Radial {
                center: (v[0] as f64, v[1] as f64),
                rotation: v[2] as f64,
                radius_y: v[3] as f64,
                radius_x: v[4] as f64,
                reserved: v[5] as f64,
                inner: v[6] as f64,
                outside: *outside != 0,
            };
        }
        match r.map {
            Some(Some(value)) => Mask::Uniform { value },
            Some(None) => unsupported("painted or colour range mask"),
            None => unsupported("unknown mask"),
        }
    }
}

/// The `.comask` files that may hold a photograph's masks: in a session,
/// `CaptureOne/Settings*/<file>.comask` beside the photograph; in a
/// catalog, `Adjustments/**/<file>.comask` in the catalog's folder.
pub fn candidates(photo: &Path, library: &Path) -> Vec<PathBuf> {
    let Some(name) = photo.file_name() else {
        return Vec::new();
    };
    let mut wanted = name.to_os_string();
    wanted.push(".comask");
    let mut out = Vec::new();
    if let Some(folder) = photo.parent()
        && let Ok(entries) = std::fs::read_dir(folder.join("CaptureOne"))
    {
        for e in entries.flatten() {
            if e.file_name().to_string_lossy().starts_with("Settings") {
                let p = e.path().join(&wanted);
                if p.is_file() {
                    out.push(p);
                }
            }
        }
    }
    let mut stack = vec![library.join("Adjustments")];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if e.file_type().is_ok_and(|t| t.is_dir()) {
                stack.push(p);
            } else if e.file_name() == wanted.as_os_str() {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Encodes a length as the file does.
    pub fn len(n: usize) -> Vec<u8> {
        if n < 0x40 {
            return vec![n as u8];
        }
        let mut digits = Vec::new();
        let mut v = n;
        while v > 0 {
            digits.push((v & 0x3f) as u8);
            v >>= 6;
        }
        digits.reverse();
        let last = digits.len() - 1;
        digits[0] |= 0x40;
        digits[last] |= 0x40;
        digits
    }

    pub fn field(key: &str, value: &[u8]) -> Vec<u8> {
        let mut out = vec![key.len() as u8];
        out.extend(key.as_bytes());
        out.extend([0x00, 0x22, 0xff]);
        out.extend(len(value.len()));
        out.extend(value);
        out.extend([0xfe, 0x21]);
        out
    }

    pub fn map(tiles: &[u8]) -> Vec<u8> {
        let mut out = vec![3];
        out.extend(b"Map");
        out.extend([0x00, 0x00]);
        out.extend(len(tiles.len()));
        out.extend(tiles);
        out
    }

    fn f32s(v: &[f32]) -> Vec<u8> {
        v.iter().flat_map(|f| f.to_le_bytes()).collect()
    }

    pub fn record(index: u32, uuid: &str, kind: u8, shape: Vec<u8>) -> Vec<u8> {
        let mut out = field("MaskIndex", &index.to_le_bytes());
        out.extend([8]);
        out.extend(b"UserInfo");
        out.extend([0x00, 0x22, 0x00]);
        out.extend(field("Info", uuid.as_bytes()));
        out.extend(field("Name", b""));
        out.extend(field("MaskType", &[kind, 0, 0, 0]));
        out.extend(field(
            "CompParam",
            &f32s(&[f32::from_bits(1), 1.0, 0.0, 0.0]),
        ));
        out.extend(shape);
        out
    }

    /// A `.comask` of a 7360 × 4912 frame with the given masks.
    pub fn file(records: &[Vec<u8>]) -> Vec<u8> {
        let mut out = vec![0x04];
        out.extend(b"XCon");
        out.extend([0xc0, 0x42, 0x06, 0xbe, 0xba, 0xfe, 0xca]);
        out.extend([8]);
        out.extend(b"MaskInfo");
        out.extend([0x00, 0x22, 0x00]);
        let mut hdr = Vec::new();
        for v in [2u32, 7360, 4912, 12, 0, 0] {
            hdr.extend(v.to_le_bytes());
        }
        out.extend(field("Hdr", &hdr));
        for r in records {
            out.extend(r);
        }
        out.extend(field("Blk", &[]));
        out
    }

    pub fn linear(uuid: &str, start: (f32, f32), end: (f32, f32)) -> Vec<u8> {
        record(
            0,
            uuid,
            3,
            field(
                "LinearGradient",
                &f32s(&[end.0, end.1, start.0, start.1, 0.5]),
            ),
        )
    }

    pub fn radial(uuid: &str, outside: bool) -> Vec<u8> {
        let mut v = f32s(&[4043.8, 2192.6, 0.0, 861.0, 1288.0, 0.0, 0.5]);
        v.extend((outside as i32).to_le_bytes());
        record(1, uuid, 4, field("RadialGradient", &v))
    }

    pub fn filled(uuid: &str, value: u8) -> Vec<u8> {
        let mut tiles = vec![0x00, value];
        tiles.extend(std::iter::repeat_n(0x01, 8854));
        record(2, uuid, 0, map(&tiles))
    }

    pub fn painted(uuid: &str) -> Vec<u8> {
        let mut tiles = vec![0x09, 0x00, 0x00];
        tiles.extend(std::iter::repeat_n(0x01, 8854));
        record(3, uuid, 0, map(&tiles))
    }

    pub fn luma(uuid: &str) -> Vec<u8> {
        let mut out = record(4, uuid, 0xff, field("CompositeMask", &2u32.to_le_bytes()));
        out.extend(filled(uuid, 255));
        let mut range = f32s(&[60.0, 170.0, 30.0, 210.0, 0.0, 50.0]);
        range.extend(0i32.to_le_bytes());
        let mut shape = field("LumaRange", &range);
        // The composite's cached result: compressed tiles.
        let mut tiles = vec![0x09, 0x00, 0x05];
        tiles.extend(std::iter::repeat_n(0x08, 8854));
        shape.extend(map(&tiles));
        out.extend(record(1, uuid, 1, shape));
        out
    }

    #[test]
    fn lengths_read_as_written() {
        for n in [0, 5, 63, 64, 4095, 8856, 9052, 6_725_560] {
            assert_eq!(varint(&len(n), 0), Some((n, len(n).len())), "{n}");
        }
        // As Capture One wrote them.
        assert_eq!(varint(&[0x42, 0x0a, 0x58], 0), Some((8856, 3)));
        assert_eq!(varint(&[0x59, 0x29, 0x3e, 0x78], 0), Some((6_725_560, 4)));
    }

    #[test]
    fn shapes_read_from_a_mask_file() {
        let d = file(&[
            linear("L", (3267.8, 562.5), (3783.7, 2538.1)),
            radial("R", true),
            filled("F", 255),
            filled("C", 0),
            painted("P"),
            luma("Y"),
        ]);
        let f = MaskFile::parse(&d).unwrap();
        assert_eq!(f.frame, (7360.0, 4912.0));
        let Mask::Linear {
            start,
            end,
            midpoint,
        } = f.mask("L")
        else {
            panic!("{:?}", f.mask("L"));
        };
        assert!((start.0 - 3267.8).abs() < 0.01 && (end.1 - 2538.1).abs() < 0.01);
        assert_eq!(midpoint, 0.5);
        let Mask::Radial {
            radius_x,
            radius_y,
            inner,
            outside,
            ..
        } = f.mask("R")
        else {
            panic!();
        };
        assert_eq!(
            (radius_x, radius_y, inner, outside),
            (1288.0, 861.0, 0.5, true)
        );
        assert_eq!(f.mask("F"), Mask::Uniform { value: 255 });
        assert_eq!(f.mask("C"), Mask::Uniform { value: 0 });
        assert!(matches!(f.mask("P"), Mask::Unsupported { .. }));
        assert_eq!(
            f.mask("Y"),
            Mask::LumaRange {
                base: 255,
                low: 60.0,
                high: 170.0,
                falloff_low: 30.0,
                falloff_high: 210.0,
                radius: 0.0,
                sensitivity: 50.0,
                invert: false,
            }
        );
        assert!(matches!(f.mask("missing"), Mask::Unsupported { .. }));
        assert!(f.has("y") && !f.has("Z"));
    }

    #[test]
    fn not_a_mask_file() {
        assert!(MaskFile::parse(b"hello").is_none());
        assert!(MaskFile::parse(&[]).is_none());
    }
}

/// A real `.comask` (`C1_COMASK`): prints every mask it holds.
#[cfg(test)]
#[test]
#[ignore = "needs a Capture One mask file (C1_COMASK)"]
fn print_real_mask_file() {
    let path = std::env::var("C1_COMASK").expect("C1_COMASK");
    let file = MaskFile::parse(&std::fs::read(path).unwrap()).expect("a mask file");
    println!("frame {:?}", file.frame);
    let mut seen = std::collections::HashSet::new();
    for r in &file.records {
        if seen.insert(r.uuid.clone()) {
            println!("{} {:?}", r.uuid, file.mask(&r.uuid));
        }
    }
}
