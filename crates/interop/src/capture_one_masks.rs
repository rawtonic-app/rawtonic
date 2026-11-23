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
//! - `Map`: a raster of 64 × 64 px tiles over the frame, row by row, one
//!   code per tile: `00 v` a uniform tile of value v, `01` another tile of
//!   the last uniform value, `09 hi lo` the compressed tile number hi·256 +
//!   lo, `08` the one after the last. Brushes, colour ranges (Color Editor
//!   "Create masked layer"), AI masks and rasterized masks are such rasters.
//! - no shape: a layer never painted (an empty mask; notes:
//!   benchmark-results/c1q1-2026-11-21/REPORT.md).
//! - `Blk` (after the last mask): `Hdr` (the number of compressed tiles),
//!   `Idx` (each tile's size, u16) and `dat` (the tiles back to back), shared
//!   by all masks of the file; `capture_one_tiles` reads a tile.
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::capture_one_tiles;

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
    /// A painted, colour range or AI mask: Capture One's raster.
    Raster {
        raster: Raster,
        /// Made by Capture One's AI selection (`MaskGenerator`).
        ai: bool,
    },
    /// A filled or painted mask limited to a Luma Range (0–255 scale).
    LumaRange {
        base: Coverage,
        low: f64,
        high: f64,
        falloff_low: f64,
        falloff_high: f64,
        radius: f64,
        sensitivity: f64,
        invert: bool,
    },
    /// A layer made and never painted: its record holds no shape (no
    /// effect anywhere, as in Capture One).
    Empty,
    /// No mask record for the layer in the photograph's mask files.
    Missing,
    /// A mask in a form not read (named for the import report).
    Unsupported { what: String },
}

/// What a Luma Range limits.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum Coverage {
    /// The same value (0–255) everywhere.
    Uniform { value: u8 },
    /// A painted raster.
    Raster { raster: Raster },
}

/// The compressed tiles of a mask file, shared by its masks.
#[derive(Debug, Default, PartialEq)]
struct Blk {
    /// Where each tile starts in `data`, and where the last ends.
    offsets: Vec<usize>,
    data: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Tile {
    Uniform(u8),
    Coded(usize),
}

/// A raster mask as Capture One stores it: 64 × 64 px tiles over the frame,
/// decoded on demand (`values`).
#[derive(Clone, PartialEq, serde::Serialize)]
pub struct Raster {
    pub width: usize,
    pub height: usize,
    #[serde(skip)]
    tiles: Vec<Tile>,
    #[serde(skip)]
    blk: Arc<Blk>,
}

impl std::fmt::Debug for Raster {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Raster({} × {})", self.width, self.height)
    }
}

impl Raster {
    /// The mask's values (0–255), row by row over `width` × `height`, or
    /// None when a tile cannot be read.
    pub fn values(&self) -> Option<Vec<u8>> {
        const T: usize = capture_one_tiles::TILE;
        let across = self.width.div_ceil(T);
        let mut out = vec![0u8; self.width * self.height];
        for (k, tile) in self.tiles.iter().enumerate() {
            let (x0, y0) = ((k % across) * T, (k / across) * T);
            let fill = match tile {
                Tile::Uniform(v) => *v,
                Tile::Coded(_) => 0,
            };
            let decoded = match tile {
                Tile::Uniform(_) => None,
                Tile::Coded(n) => {
                    Some(capture_one_tiles::decode(self.blk.data.get(
                        *self.blk.offsets.get(*n)?..*self.blk.offsets.get(*n + 1)?,
                    )?)?)
                }
            };
            for y in y0..(y0 + T).min(self.height) {
                for x in x0..(x0 + T).min(self.width) {
                    out[y * self.width + x] =
                        decoded.as_ref().map_or(fill, |d| d[(y - y0) * T + x - x0]);
                }
            }
        }
        Some(out)
    }

    /// A raster of `values` (row by row over `width` × `height`), stored
    /// as Capture One stores one (`capture_one_tiles::encode`).
    #[cfg(any(test, feature = "test-support"))]
    pub fn from_values(width: usize, height: usize, values: &[u8]) -> Self {
        const T: usize = capture_one_tiles::TILE;
        assert_eq!(values.len(), width * height);
        let (mut tiles, mut blk) = (
            Vec::new(),
            Blk {
                offsets: vec![0],
                data: Vec::new(),
            },
        );
        for ty in 0..height.div_ceil(T) {
            for tx in 0..width.div_ceil(T) {
                let mut tile = vec![0u8; T * T];
                for y in 0..T.min(height - ty * T) {
                    for x in 0..T.min(width - tx * T) {
                        tile[y * T + x] = values[(ty * T + y) * width + tx * T + x];
                    }
                }
                if tile.iter().all(|&v| v == tile[0]) {
                    tiles.push(Tile::Uniform(tile[0]));
                } else {
                    tiles.push(Tile::Coded(blk.offsets.len() - 1));
                    blk.data.extend(capture_one_tiles::encode(&tile, 16));
                    blk.offsets.push(blk.data.len());
                }
            }
        }
        Raster {
            width,
            height,
            tiles,
            blk: Arc::new(blk),
        }
    }

    /// Whether every tile is one of a single value (then `Some(value)`).
    fn uniform(&self) -> Option<u8> {
        let mut seen = None;
        for t in &self.tiles {
            match (t, seen) {
                (Tile::Uniform(v), None) => seen = Some(*v),
                (Tile::Uniform(v), Some(s)) if *v == s => {}
                _ => return None,
            }
        }
        seen
    }
}

/// The masks of one `.comask` file.
#[derive(Debug, Clone, Default)]
pub struct MaskFile {
    /// Width and height of the frame the masks are drawn in.
    pub frame: (f64, f64),
    records: Vec<Record>,
    blk: Arc<Blk>,
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
    /// The `Map` field's bytes.
    map: Option<Vec<u8>>,
    /// A field this reader does not know (a shape it cannot read).
    unknown: bool,
}

/// One field, with no value when its declared length exceeds the file.
struct Field<'a> {
    key: &'a str,
    value: Option<&'a [u8]>,
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
    // Keys start with a capital, but for the tiles' `dat`.
    if !key.iter().all(u8::is_ascii_alphabetic) || !(key[0].is_ascii_uppercase() || key == b"dat") {
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
        return Some((
            Field {
                key,
                value: Some(&[]),
            },
            j,
        ));
    }
    while *d.get(j)? == 0xff {
        j += 1;
    }
    let (n, j) = varint(d, j)?;
    let value = d.get(j..)?.get(..n);
    let next = value.map_or(j, |bytes| j + bytes.len());
    Some((Field { key, value }, next))
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

/// The tiles of a `Map`, or None when its codes are not ones read.
fn map_tiles(m: &[u8]) -> Option<Vec<Tile>> {
    let (mut i, mut current, mut next) = (0, 0u8, None::<usize>);
    let mut out = Vec::new();
    while i < m.len() {
        match m[i] {
            0x00 => {
                current = *m.get(i + 1)?;
                out.push(Tile::Uniform(current));
                i += 2;
            }
            0x01 => {
                out.push(Tile::Uniform(current));
                i += 1;
            }
            0x09 => {
                let n = ((*m.get(i + 1)? as usize) << 8) | *m.get(i + 2)? as usize;
                out.push(Tile::Coded(n));
                next = Some(n + 1);
                i += 3;
            }
            0x08 => {
                let n = next?;
                out.push(Tile::Coded(n));
                next = Some(n + 1);
                i += 1;
            }
            _ => return None,
        }
    }
    Some(out)
}

impl MaskFile {
    pub fn parse(d: &[u8]) -> Option<Self> {
        if d.get(1..5) != Some(b"XCon") {
            return None;
        }
        let mut file = MaskFile::default();
        let mut i = 5;
        let mut current: Option<Record> = None;
        let mut in_blk = false;
        let (mut sizes, mut data) = (None::<Vec<usize>>, None::<&[u8]>);
        while i < d.len() {
            let Some((f, next)) = field_at(d, i) else {
                i += 1;
                continue;
            };
            let Some(value) = f.value else {
                // Keep scanning for the next record or Blk, but this
                // record contained an unreadable field.
                if !in_blk && let Some(r) = current.as_mut() {
                    r.unknown = true;
                }
                i = next;
                continue;
            };
            if in_blk {
                match f.key {
                    "Idx" => {
                        sizes = Some(
                            value
                                .as_chunks::<2>()
                                .0
                                .iter()
                                .map(|c| u16::from_le_bytes(*c) as usize)
                                .collect(),
                        );
                    }
                    "dat" => data = Some(value),
                    _ => {}
                }
                i = next;
                continue;
            }
            match f.key {
                // The compressed tiles follow the last mask.
                "Blk" => in_blk = true,
                "Hdr" if file.frame == (0.0, 0.0) => {
                    if let (Some(w), Some(h)) = (int_at(value, 1), int_at(value, 2)) {
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
                            "Info" => r.uuid = String::from_utf8_lossy(value).into_owned(),
                            "Name" => r.name = String::from_utf8_lossy(value).into_owned(),
                            "MaskType" => r.mask_type = value.get(..4)?.try_into().ok(),
                            // A shape field that does not read is a shape
                            // not read, never an empty mask.
                            "CompositeMask" => {
                                r.composite = int_at(value, 0).map(|n| n as u32);
                                r.unknown |= r.composite.is_none();
                            }
                            "LinearGradient" => {
                                r.linear = floats(value, 5);
                                r.unknown |= r.linear.is_none();
                            }
                            "RadialGradient" => {
                                r.radial = floats(value, 7).zip(int_at(value, 7));
                                r.unknown |= r.radial.is_none();
                            }
                            "LumaRange" => {
                                r.luma = floats(value, 6).zip(int_at(value, 6));
                                r.unknown |= r.luma.is_none();
                            }
                            "Map" => r.map = Some(value.to_vec()),
                            "UserInfo" | "CompParam" => {}
                            _ => r.unknown = true,
                        }
                    }
                }
            }
            i = next;
        }
        // Capture One ends the file with the shared tiles: a file cut short
        // may have cut the last mask's shape off, so it is not read as one
        // never painted.
        if !in_blk && let Some(r) = current.as_mut() {
            r.unknown = true;
        }
        file.records.extend(current);
        if let (Some(sizes), Some(data)) = (sizes, data) {
            let mut offsets = vec![0];
            for s in sizes {
                offsets.push(offsets.last().unwrap() + s);
            }
            if *offsets.last().unwrap() <= data.len() {
                file.blk = Arc::new(Blk {
                    offsets,
                    data: data.to_vec(),
                });
            }
        }
        (file.frame.0 > 0.0 && file.frame.1 > 0.0).then_some(file)
    }

    /// A record's raster, when its tiles cover the frame and every one reads.
    fn raster(&self, r: &Record) -> Result<Raster, &'static str> {
        let tiles = r.map.as_deref().and_then(map_tiles).ok_or("unknown mask")?;
        const T: usize = capture_one_tiles::TILE;
        let (width, height) = (self.frame.0 as usize, self.frame.1 as usize);
        if tiles.len() != width.div_ceil(T) * height.div_ceil(T) {
            return Err("mask raster of another size");
        }
        let raster = Raster {
            width,
            height,
            tiles,
            blk: self.blk.clone(),
        };
        let readable = raster.tiles.iter().all(|t| match t {
            Tile::Uniform(_) => true,
            Tile::Coded(n) => self
                .blk
                .offsets
                .get(*n..*n + 2)
                .and_then(|o| self.blk.data.get(o[0]..o[1]))
                .and_then(capture_one_tiles::decode)
                .is_some(),
        });
        if readable {
            Ok(raster)
        } else {
            Err("mask tiles not readable")
        }
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
            return Mask::Missing;
        };
        let r = &self.records[at];
        if let Some(n) = r.composite {
            let parts = self.records.get(at + 1..at + 1 + n as usize);
            return match parts {
                Some([base, range]) if base.map.is_some() => match (self.raster(base), &range.luma)
                {
                    (Err(why), _) => unsupported(why),
                    (Ok(raster), Some((v, invert))) if v.len() == 6 => Mask::LumaRange {
                        base: match raster.uniform() {
                            Some(value) => Coverage::Uniform { value },
                            None => Coverage::Raster { raster },
                        },
                        low: v[0] as f64,
                        high: v[1] as f64,
                        falloff_low: v[2] as f64,
                        falloff_high: v[3] as f64,
                        radius: v[4] as f64,
                        sensitivity: v[5] as f64,
                        invert: *invert != 0,
                    },
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
        if r.map.is_none() {
            // Capture One's record of a layer never painted: its index,
            // UUID, name, type and composition parameters, nothing else.
            return if r.unknown || r.luma.is_some() {
                unsupported("unknown mask")
            } else {
                Mask::Empty
            };
        }
        match self.raster(r) {
            Ok(raster) => match raster.uniform() {
                Some(value) => Mask::Uniform { value },
                None => Mask::Raster {
                    raster,
                    ai: r.name.contains("MaskGenerator"),
                },
            },
            Err(why) => unsupported(why),
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
        file_with_tiles(records, &[])
    }

    /// `file` with compressed tiles (`capture_one_tiles::encode`) in its
    /// `Blk`, numbered from 0.
    pub fn file_with_tiles(records: &[Vec<u8>], tiles: &[Vec<u8>]) -> Vec<u8> {
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
        out.extend([3]);
        out.extend(b"Blk");
        out.extend([0x00, 0x22, 0x00]);
        let mut hdr = (tiles.len() as u32).to_le_bytes().to_vec();
        hdr.extend([0; 12]);
        out.extend(field("Hdr", &hdr));
        let sizes: Vec<u8> = tiles
            .iter()
            .flat_map(|t| (t.len() as u16).to_le_bytes())
            .collect();
        out.extend(field("Idx", &sizes));
        out.extend(field("dat", &tiles.concat()));
        out
    }

    /// A raster mask: tile k (row by row over the 115 × 77 tiles) is the
    /// compressed tile `coded[k]` where given, else uniform `fill`.
    pub fn raster(uuid: &str, fill: u8, coded: &[(usize, usize)]) -> Vec<u8> {
        let mut tiles = Vec::new();
        let mut last = None;
        for k in 0..115 * 77 {
            match coded.iter().find(|c| c.0 == k) {
                Some(&(_, n)) if last == Some(n.wrapping_sub(1)) => tiles.push(0x08),
                Some(&(_, n)) => tiles.extend([0x09, (n >> 8) as u8, n as u8]),
                None if k == 0 || last.is_some() => tiles.extend([0x00, fill]),
                None => tiles.push(0x01),
            }
            last = coded.iter().find(|c| c.0 == k).map(|c| c.1);
        }
        record(5, uuid, 0, map(&tiles))
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
        // A compressed tile the file does not hold.
        assert!(matches!(f.mask("P"), Mask::Unsupported { .. }));
        assert_eq!(
            f.mask("Y"),
            Mask::LumaRange {
                base: Coverage::Uniform { value: 255 },
                low: 60.0,
                high: 170.0,
                falloff_low: 30.0,
                falloff_high: 210.0,
                radius: 0.0,
                sensitivity: 50.0,
                invert: false,
            }
        );
        assert_eq!(f.mask("missing"), Mask::Missing);
        assert!(f.has("y") && !f.has("Z"));
    }

    /// A disc of 200 in a tile of 0.
    fn disc_tile() -> Vec<u8> {
        (0..64 * 64)
            .map(|i| {
                let (y, x) = ((i / 64) as f64 - 31.5, (i % 64) as f64 - 31.5);
                if x * x + y * y < 400.0 { 200 } else { 0 }
            })
            .collect()
    }

    #[test]
    fn painted_rasters_read_at_their_place() {
        use crate::capture_one_tiles::encode;
        let disc = disc_tile();
        let ramp: Vec<u8> = (0..64 * 64).map(|i| (i % 64 * 4) as u8).collect();
        // Tile 0 (top left), tile 116 (second row, second column) and the
        // last tile, which the frame cuts to 64 × 48.
        let d = file_with_tiles(
            &[
                raster("P", 0, &[(0, 0), (116, 1), (115 * 77 - 1, 2)]),
                raster("Q", 255, &[(5, 1), (6, 2)]),
            ],
            &[encode(&disc, 16), encode(&ramp, 64), encode(&disc, 4095)],
        );
        let f = MaskFile::parse(&d).unwrap();
        let Mask::Raster { raster, ai } = f.mask("P") else {
            panic!("{:?}", f.mask("P"));
        };
        assert!(!ai);
        assert_eq!((raster.width, raster.height), (7360, 4912));
        let v = raster.values().unwrap();
        let at = |x: usize, y: usize| v[y * 7360 + x];
        assert_eq!((at(31, 31), at(0, 0), at(63, 63)), (200, 0, 0));
        assert_eq!((at(64 + 10, 64 + 3), at(64 + 63, 64)), (40, 252));
        assert_eq!(at(7360 - 64 + 31, 4912 - 48 + 31 - 16), 200);
        assert_eq!(at(3000, 3000), 0);
        let Mask::Raster { raster, .. } = f.mask("Q") else {
            panic!();
        };
        let v = raster.values().unwrap();
        assert_eq!(
            (
                v[0],
                v[5 * 64 + 1],
                v[5 * 64 + 63 + 7360],
                v[31 * 7360 + 6 * 64 + 31]
            ),
            (255, 4, 252, 200)
        );
        // A filled raster stays a filled mask.
        let d = file_with_tiles(&[tests::raster("F", 128, &[])], &[]);
        assert_eq!(
            MaskFile::parse(&d).unwrap().mask("F"),
            Mask::Uniform { value: 128 }
        );
    }

    #[test]
    fn painted_luma_ranges_and_unreadable_tiles() {
        use crate::capture_one_tiles::encode;
        let mut composite = record(4, "Y", 0xff, field("CompositeMask", &2u32.to_le_bytes()));
        composite.extend(raster("Y", 0, &[(40, 0)]));
        let mut range = f32s(&[60.0, 170.0, 30.0, 210.0, 0.0, 50.0]);
        range.extend(0i32.to_le_bytes());
        composite.extend(record(1, "Y", 1, field("LumaRange", &range)));
        let d = file_with_tiles(
            &[
                composite,
                raster("Bad", 0, &[(3, 1)]),
                raster("Gone", 0, &[(3, 9)]),
            ],
            &[encode(&disc_tile(), 16), vec![0x02, 0x00, 0x91]],
        );
        let f = MaskFile::parse(&d).unwrap();
        let Mask::LumaRange {
            base: Coverage::Raster { raster },
            low,
            ..
        } = f.mask("Y")
        else {
            panic!("{:?}", f.mask("Y"));
        };
        assert_eq!(low, 60.0);
        assert_eq!(raster.values().unwrap()[31 * 7360 + 40 * 64 + 31], 200);
        // A tile the decoder does not know, and one missing from the file:
        // the mask is not read (and named), never guessed.
        for uuid in ["Bad", "Gone"] {
            assert_eq!(
                f.mask(uuid),
                Mask::Unsupported {
                    what: "mask tiles not readable".into()
                },
                "{uuid}"
            );
        }
    }

    /// A layer made and never painted: Capture One writes its record with
    /// no shape (C1Q1: "Anpassungsebene 1" on every photo). Its mask is
    /// empty, not unknown; a layer without any record is missing.
    #[test]
    fn never_painted_masks_read_as_empty() {
        let mut odd = record(6, "O", 0, Vec::new());
        odd.extend(field("SomeNewShape", &f32s(&[1.0, 2.0])));
        let short = record(7, "S", 3, field("LinearGradient", &f32s(&[1.0, 2.0])));
        let f = MaskFile::parse(&file(&[
            record(5, "E", 0, Vec::new()),
            odd,
            short,
            filled("F", 255),
        ]))
        .unwrap();
        assert_eq!(f.mask("E"), Mask::Empty);
        assert_eq!(f.mask("F"), Mask::Uniform { value: 255 });
        // A shape this reader does not know is not taken for an empty mask.
        assert!(matches!(f.mask("O"), Mask::Unsupported { .. }));
        // Nor is a gradient too short to read, nor a Luma Range alone.
        assert!(matches!(f.mask("S"), Mask::Unsupported { .. }));
        // The shape length runs past EOF even though a Blk header follows.
        let key = "LinearGradient";
        let mut truncated = vec![key.len() as u8];
        truncated.extend(key.as_bytes());
        truncated.extend([0x00, 0x22, 0xff]);
        truncated.extend(len(1024));
        let f = MaskFile::parse(&file(&[
            record(5, "E", 0, Vec::new()),
            record(9, "T", 3, truncated),
        ]))
        .unwrap();
        assert_eq!(f.mask("E"), Mask::Empty);
        assert!(matches!(f.mask("T"), Mask::Unsupported { .. }));
        let mut range = f32s(&[60.0, 170.0, 30.0, 210.0, 0.0, 50.0]);
        range.extend(0i32.to_le_bytes());
        let lone = record(8, "R", 1, field("LumaRange", &range));
        let f = MaskFile::parse(&file(&[lone])).unwrap();
        assert!(matches!(f.mask("R"), Mask::Unsupported { .. }));
        // A file cut off in its last mask (no tiles after it).
        let mut cut = file(&[record(5, "E", 0, Vec::new())]);
        let end = cut.windows(3).rposition(|w| w == b"Blk").unwrap() - 1;
        cut.truncate(end);
        let f = MaskFile::parse(&cut).unwrap();
        assert!(matches!(f.mask("E"), Mask::Unsupported { .. }));
        assert_eq!(f.mask("nowhere"), Mask::Missing);
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
            match file.mask(&r.uuid) {
                Mask::Raster { raster, ai } => {
                    let v = raster.values().expect("readable");
                    let sum: u64 = v.iter().map(|&x| x as u64).sum();
                    println!("{} raster ai={ai} sum {sum}", r.uuid);
                }
                m => println!("{} {m:?}", r.uuid),
            }
        }
    }
}
