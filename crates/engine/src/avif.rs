//! AVIF sources: AV1 still images in a HEIF container (AV1 Image File Format
//! 1.1, ISO/IEC 23008-12).
//!
//! The container is read by avif-parse (primary and alpha items of single
//! images), the AV1 data is decoded by rav1d (the Rust port of dav1d)
//! without assembly, so the build needs no NASM or C toolchain on any
//! platform. What avif-parse does not report is read from the item
//! structure here (`Meta`): the primary item's colour (`colr`: ICC profile
//! and/or `nclx` code points), clean aperture (`clap`) and orientation
//! (`irot`, `imir`), and grid images (`grid` items of AV1 tiles, with an
//! alpha grid or item). The YCbCr → RGB conversion follows ITU-T H.273 and
//! yields the encoded (non-linear) values in 0…1 as `f32`, at any bit depth
//! (8, 10, 12) and chroma layout (4:0:0, 4:2:0, 4:2:2, 4:4:4). HDR
//! transfers (PQ, HLG) become linear light with diffuse white at 1.0
//! (`hdr_to_working_space`). Layered items decode at their operating point
//! (`a1op`) to the selected (`lsel`) or highest spatial layer.
//!
//! Not supported (the load fails with a message, or the property is not
//! applied): image sequences beyond their primary item, gain maps (the base
//! image shows), a clean aperture off whole pixels, and constant-luminance /
//! ICtCp matrices.

use anyhow::{Result, anyhow, bail};
use image::{DynamicImage, ImageBuffer, Rgb, Rgba};
use rav1d::include::dav1d::data::Dav1dData;
use rav1d::include::dav1d::dav1d::{Dav1dContext, Dav1dSettings};
use rav1d::include::dav1d::headers::{
    DAV1D_CHR_COLOCATED, DAV1D_CHR_VERTICAL, DAV1D_PIXEL_LAYOUT_I400, DAV1D_PIXEL_LAYOUT_I420,
    DAV1D_PIXEL_LAYOUT_I422, DAV1D_PIXEL_LAYOUT_I444,
};
use rav1d::include::dav1d::picture::Dav1dPicture;
use rav1d::src::lib::{
    dav1d_close, dav1d_data_create, dav1d_data_unref, dav1d_default_settings, dav1d_get_picture,
    dav1d_open, dav1d_picture_unref, dav1d_send_data,
};
use rayon::prelude::*;
use std::borrow::Cow;
use std::mem::MaybeUninit;
use std::ptr::NonNull;

/// Whether `bytes` are an AVIF file: an ISO BMFF file type box naming the
/// `avif` (still image) or `avis` (image sequence) brand.
pub fn is_avif(bytes: &[u8]) -> bool {
    if bytes.get(4..8) != Some(b"ftyp") {
        return false;
    }
    let Some(size) = bytes
        .get(0..4)
        .map(|s| u32::from_be_bytes(s.try_into().unwrap()))
    else {
        return false;
    };
    let end = (size as usize).min(bytes.len());
    // Major brand at 8, minor version at 12, compatible brands from 16.
    let brands = bytes.get(8..12).into_iter().chain(
        bytes
            .get(16..end)
            .unwrap_or(&[])
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| b.as_slice()),
    );
    brands.into_iter().any(|b| b == b"avif" || b == b"avis")
}

/// H.273 colour code points (`nclx`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cicp {
    pub primaries: u16,
    pub transfer: u16,
    pub matrix: u16,
    pub full_range: bool,
}

/// A decoded AVIF image before colour management and orientation.
pub struct AvifImage {
    /// RGB(A) as encoded, in 0…1 (`Rgb32F`, or `Rgba32F` with an alpha item).
    pub image: DynamicImage,
    /// The embedded ICC profile (`colr` `prof`/`rICC`).
    pub icc: Option<Vec<u8>>,
    /// The colour code points: the `colr` `nclx` property, else the AV1
    /// sequence header's.
    pub cicp: Cicp,
    /// The EXIF orientation (1–8) equivalent to the `irot`/`imir`
    /// properties; HEIF readers apply these, not the EXIF tag.
    pub orientation: u16,
}

/// An item's colour, crop and orientation properties.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Properties {
    icc: Option<Vec<u8>>,
    nclx: Option<Cicp>,
    /// Anticlockwise quarter turns (`irot`).
    rotation: u8,
    /// `imir` axis (ISO/IEC 23008-12:2022 6.5.12): 0 exchanges top and
    /// bottom, 1 left and right (as libavif and libheif read it).
    mirror: Option<u8>,
    /// The clean aperture (`clap`): width N/D, height N/D, horizontal
    /// offset N/D, vertical offset N/D (the offsets' numerators signed).
    clap: Option<[i64; 8]>,
    layering: Layering,
}

/// Which layers of a layered AV1 item are shown (AV1 Image File Format
/// 2.2.3: `a1op`, `lsel`).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Layering {
    /// The operating point decoded (`a1op`; 0, all layers, by default).
    operating_point: u8,
    /// The spatial layer output (`lsel`), else the highest one of the
    /// operating point (`lsel` 0xFFFF or none).
    layer: Option<u16>,
}

/// ISO BMFF boxes in `data`: (type, payload). Stops at the first malformed
/// header.
fn boxes(data: &[u8]) -> impl Iterator<Item = ([u8; 4], &[u8])> {
    let mut at = 0usize;
    std::iter::from_fn(move || {
        let header = data.get(at..at.checked_add(8)?)?;
        let size32 = u32::from_be_bytes(header[0..4].try_into().unwrap()) as usize;
        let kind: [u8; 4] = header[4..8].try_into().unwrap();
        let (size, header_len) = match size32 {
            0 => (data.len() - at, 8),
            1 => {
                let large = data.get(at + 8..at + 16)?;
                (
                    usize::try_from(u64::from_be_bytes(large.try_into().unwrap())).ok()?,
                    16,
                )
            }
            n => (n, 8),
        };
        if size < header_len {
            return None;
        }
        let payload = data.get(at + header_len..at.checked_add(size)?)?;
        at += size;
        Some((kind, payload))
    })
}

fn child<'a>(data: &'a [u8], kind: &[u8; 4]) -> Option<&'a [u8]> {
    boxes(data).find(|(k, _)| k == kind).map(|(_, p)| p)
}

fn be16(data: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(data.get(at..at + 2)?.try_into().ok()?))
}

fn be32(data: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(data.get(at..at + 4)?.try_into().ok()?))
}

/// An unsigned big-endian field of 0, 4 or 8 bytes (`iloc` field sizes).
fn be_sized(data: &[u8], at: usize, size: usize) -> Option<u64> {
    match size {
        0 => Some(0),
        4 => be32(data, at).map(u64::from),
        8 => Some(u64::from_be_bytes(data.get(at..at + 8)?.try_into().ok()?)),
        _ => None,
    }
}

/// An item reference (`iref`): type, from item, to items.
type Reference = ([u8; 4], u32, Vec<u32>);

/// Where an item's data lies (`iloc`): extents (offset, length; length 0
/// runs to the end) in the file (construction method 0) or in `idat` (1).
#[derive(Debug, Clone)]
struct Location {
    method: u8,
    extents: Vec<(u64, u64)>,
}

/// The item structure of a HEIF `meta` box (ISO/IEC 23008-12, ISO/IEC
/// 14496-12 8.11), for what avif-parse does not report: every item's
/// properties, and the references and locations of grid tiles and of a
/// grid's alpha item.
struct Meta<'a> {
    file: &'a [u8],
    primary: u32,
    /// (item ID, item type) from `iinf`.
    items: Vec<(u32, [u8; 4])>,
    /// `ipco` boxes; property index `n` is `properties[n - 1]`.
    properties: Vec<([u8; 4], &'a [u8])>,
    /// (item ID, property indices) from `ipma`.
    associations: Vec<(u32, Vec<u16>)>,
    references: Vec<Reference>,
    locations: Vec<(u32, Location)>,
    idat: &'a [u8],
}

fn parse_ipma(iprp: &[u8]) -> Option<Vec<(u32, Vec<u16>)>> {
    let mut associations = Vec::new();
    for (kind, ipma) in boxes(iprp) {
        if &kind != b"ipma" {
            continue;
        }
        let version = *ipma.first()?;
        let wide_index = ipma.get(3)? & 1 == 1;
        let count = be32(ipma, 4)?;
        let mut at = 8;
        for _ in 0..count {
            let item = if version < 1 {
                at += 2;
                u32::from(be16(ipma, at - 2)?)
            } else {
                at += 4;
                be32(ipma, at - 4)?
            };
            let n = *ipma.get(at)?;
            at += 1;
            let mut indices = Vec::with_capacity(usize::from(n));
            for _ in 0..n {
                let index = if wide_index {
                    at += 2;
                    be16(ipma, at - 2)? & 0x7fff
                } else {
                    at += 1;
                    u16::from(*ipma.get(at - 1)? & 0x7f)
                };
                if index > 0 {
                    indices.push(index);
                }
            }
            associations.push((item, indices));
        }
    }
    Some(associations)
}

fn parse_iinf(iinf: &[u8]) -> Option<Vec<(u32, [u8; 4])>> {
    let entries = if *iinf.first()? == 0 { 6 } else { 8 };
    let mut items = Vec::new();
    for (kind, infe) in boxes(iinf.get(entries..)?) {
        if &kind != b"infe" {
            continue;
        }
        // Version 2: 16-bit ID, protection index, type; version 3: 32-bit ID.
        let (id, type_at) = match infe.first()? {
            2 => (u32::from(be16(infe, 4)?), 8),
            3 => (be32(infe, 4)?, 10),
            _ => continue,
        };
        items.push((id, infe.get(type_at..type_at + 4)?.try_into().ok()?));
    }
    Some(items)
}

fn parse_iref(iref: &[u8]) -> Option<Vec<Reference>> {
    let wide = *iref.first()? != 0;
    let size = if wide { 4 } else { 2 };
    let id = |data: &[u8], at: usize| {
        if wide {
            be32(data, at)
        } else {
            be16(data, at).map(u32::from)
        }
    };
    let mut references = Vec::new();
    for (kind, reference) in boxes(iref.get(4..)?) {
        let from = id(reference, 0)?;
        let count = usize::from(be16(reference, size)?);
        let to = (0..count)
            .map(|i| id(reference, size + 2 + i * size))
            .collect::<Option<Vec<_>>>()?;
        references.push((kind, from, to));
    }
    Some(references)
}

fn parse_iloc(iloc: &[u8]) -> Option<Vec<(u32, Location)>> {
    let version = *iloc.first()?;
    let sizes = be16(iloc, 4)?;
    let offset_size = usize::from(sizes >> 12);
    let length_size = usize::from((sizes >> 8) & 15);
    let base_size = usize::from((sizes >> 4) & 15);
    let index_size = if version == 1 || version == 2 {
        usize::from(sizes & 15)
    } else {
        0
    };
    let mut at = 6;
    let count = if version < 2 {
        at += 2;
        usize::from(be16(iloc, at - 2)?)
    } else {
        at += 4;
        be32(iloc, at - 4)? as usize
    };
    let mut locations = Vec::new();
    for _ in 0..count {
        let id = if version < 2 {
            at += 2;
            u32::from(be16(iloc, at - 2)?)
        } else {
            at += 4;
            be32(iloc, at - 4)?
        };
        let method = if version == 1 || version == 2 {
            at += 2;
            (be16(iloc, at - 2)? & 15) as u8
        } else {
            0
        };
        // Data reference index (0: this file).
        at += 2;
        let base = be_sized(iloc, at, base_size)?;
        at += base_size;
        let extent_count = be16(iloc, at)?;
        at += 2;
        let mut extents = Vec::with_capacity(usize::from(extent_count));
        for _ in 0..extent_count {
            at += index_size;
            let offset = be_sized(iloc, at, offset_size)?;
            at += offset_size;
            let length = be_sized(iloc, at, length_size)?;
            at += length_size;
            extents.push((base.checked_add(offset)?, length));
        }
        locations.push((id, Location { method, extents }));
    }
    Some(locations)
}

/// URNs of an alpha auxiliary image (`auxC`).
const ALPHA_URNS: [&[u8]; 2] = [
    b"urn:mpeg:mpegB:cicp:systems:auxiliary:alpha",
    b"urn:mpeg:hevc:2015:auxid:1",
];

/// A grid image (ISO/IEC 23008-12 6.6.2.3): tiles of equal size, row by
/// row, cropped to the output size at the right and bottom.
struct Grid<'a> {
    rows: usize,
    columns: usize,
    width: usize,
    height: usize,
    tiles: Vec<Cow<'a, [u8]>>,
    /// The tiles' layer selection (the same for every tile).
    layering: Layering,
}

/// The coded data of an image item.
enum Coded<'a> {
    /// One AV1 item.
    Single(Cow<'a, [u8]>, Layering),
    Grid(Grid<'a>),
}

impl<'a> Meta<'a> {
    fn parse(file: &'a [u8]) -> Option<Self> {
        let meta = child(file, b"meta")?.get(4..)?;
        let pitm = child(meta, b"pitm")?;
        let primary = if pitm.first()? == &0 {
            u32::from(be16(pitm, 4)?)
        } else {
            be32(pitm, 4)?
        };
        let iprp = child(meta, b"iprp")?;
        Some(Self {
            file,
            primary,
            items: child(meta, b"iinf")
                .and_then(parse_iinf)
                .unwrap_or_default(),
            properties: boxes(child(iprp, b"ipco")?).collect(),
            associations: parse_ipma(iprp)?,
            references: child(meta, b"iref")
                .and_then(parse_iref)
                .unwrap_or_default(),
            locations: child(meta, b"iloc")
                .and_then(parse_iloc)
                .unwrap_or_default(),
            idat: child(meta, b"idat").unwrap_or_default(),
        })
    }

    fn kind(&self, id: u32) -> Option<[u8; 4]> {
        self.items.iter().find(|(i, _)| *i == id).map(|(_, k)| *k)
    }

    fn is_grid(&self) -> bool {
        self.kind(self.primary) == Some(*b"grid")
    }

    /// The boxes of the properties associated with item `id`, in order.
    fn item_properties(&self, id: u32) -> impl Iterator<Item = ([u8; 4], &'a [u8])> + '_ {
        self.associations
            .iter()
            .filter(move |(item, _)| *item == id)
            .flat_map(|(_, indices)| indices)
            .filter_map(|&index| self.properties.get(usize::from(index) - 1).copied())
    }

    /// The items `from` refers to with a `kind` reference.
    fn references_from(&self, kind: &[u8; 4], from: u32) -> &[u32] {
        self.references
            .iter()
            .find(|(k, f, _)| k == kind && *f == from)
            .map_or(&[], |(_, _, to)| to)
    }

    fn properties(&self, id: u32) -> Option<Properties> {
        let mut properties = Properties::default();
        for (kind, payload) in self.item_properties(id) {
            match &kind {
                b"colr" => match payload.get(0..4) {
                    Some(b"nclx") if properties.nclx.is_none() => {
                        properties.nclx = Some(Cicp {
                            primaries: be16(payload, 4)?,
                            transfer: be16(payload, 6)?,
                            matrix: be16(payload, 8)?,
                            full_range: payload.get(10)? & 0x80 != 0,
                        });
                    }
                    Some(b"prof" | b"rICC") if properties.icc.is_none() => {
                        properties.icc = Some(payload[4..].to_vec());
                    }
                    _ => {}
                },
                b"a1op" => properties.layering.operating_point = *payload.first()?,
                b"lsel" => {
                    properties.layering.layer = Some(be16(payload, 0)?).filter(|&l| l != 0xFFFF);
                }
                b"irot" => properties.rotation = payload.first()? & 3,
                b"imir" => properties.mirror = Some(payload.first()? & 1),
                b"clap" => {
                    let mut clap = [0i64; 8];
                    for (i, value) in clap.iter_mut().enumerate() {
                        let v = be32(payload, i * 4)?;
                        // The offsets' numerators are signed.
                        *value = if i == 4 || i == 6 {
                            i64::from(v as i32)
                        } else {
                            i64::from(v)
                        };
                    }
                    properties.clap = Some(clap);
                }
                _ => {}
            }
        }
        Some(properties)
    }

    /// The primary item's properties. A grid without colour of its own
    /// takes its first tile's (libavif writes `colr` on the grid item,
    /// other writers on the tiles).
    fn primary_properties(&self) -> Option<Properties> {
        let mut properties = self.properties(self.primary)?;
        if self.is_grid() && properties.icc.is_none() && properties.nclx.is_none() {
            let tile = self.references_from(b"dimg", self.primary).first();
            if let Some(tile) = tile.and_then(|&t| self.properties(t)) {
                properties.icc = tile.icc;
                properties.nclx = tile.nclx;
            }
        }
        Some(properties)
    }

    /// An item's data: its extents, concatenated.
    fn data(&self, id: u32) -> Result<Cow<'a, [u8]>> {
        let Some((_, location)) = self.locations.iter().find(|(i, _)| *i == id) else {
            bail!("AVIF item {id} has no location");
        };
        let source = match location.method {
            0 => self.file,
            1 => self.idat,
            other => bail!("AVIF item construction method {other} is not supported"),
        };
        let extent = |&(offset, length): &(u64, u64)| -> Result<&'a [u8]> {
            let start = usize::try_from(offset).ok();
            let end = match length {
                0 => Some(source.len()),
                n => start
                    .zip(usize::try_from(n).ok())
                    .and_then(|(s, n)| s.checked_add(n)),
            };
            start
                .zip(end)
                .and_then(|(s, e)| source.get(s..e))
                .ok_or_else(|| anyhow!("AVIF item {id} lies outside the file"))
        };
        match location.extents.as_slice() {
            [single] => Ok(Cow::Borrowed(extent(single)?)),
            extents => {
                let mut data = Vec::new();
                for e in extents {
                    data.extend_from_slice(extent(e)?);
                }
                Ok(Cow::Owned(data))
            }
        }
    }

    /// The coded data of item `id`: an AV1 item or a grid of them.
    fn coded(&self, id: u32) -> Result<Coded<'a>> {
        match &self.kind(id).unwrap_or_default() {
            b"av01" => Ok(Coded::Single(self.data(id)?, self.layering(id)?)),
            b"grid" => Ok(Coded::Grid(self.grid(id)?)),
            other => bail!(
                "AVIF item type '{}' is not supported",
                String::from_utf8_lossy(other)
            ),
        }
    }

    /// An AV1 item's layer selection.
    fn layering(&self, id: u32) -> Result<Layering> {
        let layering = self.properties(id).unwrap_or_default().layering;
        // AV1 has at most 4 spatial layers and 32 operating points.
        if layering.layer.is_some_and(|l| l > 3) || layering.operating_point > 31 {
            bail!("AVIF layer selection {layering:?} is not valid");
        }
        Ok(layering)
    }

    /// A grid item's layout (`ImageGrid` in the item's data) and tiles
    /// (`dimg` references, row by row).
    fn grid(&self, id: u32) -> Result<Grid<'a>> {
        let descriptor = self.data(id)?;
        let (Some(&0), Some(&flags), Some(&rows), Some(&columns)) = (
            descriptor.first(),
            descriptor.get(1),
            descriptor.get(2),
            descriptor.get(3),
        ) else {
            bail!("AVIF grid descriptor is not supported");
        };
        let size = if flags & 1 == 1 {
            be32(&descriptor, 4).zip(be32(&descriptor, 8))
        } else {
            be16(&descriptor, 4)
                .zip(be16(&descriptor, 6))
                .map(|(w, h)| (u32::from(w), u32::from(h)))
        };
        let Some((width, height)) = size else {
            bail!("AVIF grid descriptor is truncated");
        };
        let (rows, columns) = (usize::from(rows) + 1, usize::from(columns) + 1);
        let (width, height) = (width as usize, height as usize);
        // Above a gigapixel, as for single items, the file is taken for corrupt.
        if width == 0 || height == 0 || width.saturating_mul(height) > 1 << 30 {
            bail!("AVIF grid size {width}×{height} is not supported");
        }
        let ids = self.references_from(b"dimg", id);
        if ids.len() != rows * columns {
            bail!("AVIF grid of {rows}×{columns} lists {} tiles", ids.len());
        }
        // Each cell is its own item (an invalid file otherwise, as libavif
        // reads it).
        if ids.iter().enumerate().any(|(i, t)| ids[..i].contains(t)) {
            bail!("AVIF grid lists a tile more than once");
        }
        let tiles = ids
            .iter()
            .map(|&tile| {
                if self.kind(tile) != Some(*b"av01") {
                    bail!("AVIF grid tile {tile} is not an AV1 image");
                }
                self.data(tile)
            })
            .collect::<Result<_>>()?;
        let layering = self.layering(ids[0])?;
        for &tile in &ids[1..] {
            if self.layering(tile)? != layering {
                bail!("AVIF grid tiles differ in their layer selection");
            }
        }
        Ok(Grid {
            rows,
            columns,
            width,
            height,
            tiles,
            layering,
        })
    }

    /// The primary item's alpha item: an auxiliary image (`auxl` reference
    /// to the primary item) of alpha type (`auxC`).
    fn alpha_item(&self) -> Option<u32> {
        self.references
            .iter()
            .filter(|(kind, _, to)| kind == b"auxl" && to.contains(&self.primary))
            .map(|(_, from, _)| *from)
            .find(|&id| {
                self.item_properties(id).any(|(kind, payload)| {
                    let urn = payload.get(4..).unwrap_or_default();
                    let urn = urn.split(|&b| b == 0).next().unwrap_or_default();
                    &kind == b"auxC" && ALPHA_URNS.contains(&urn)
                })
            })
    }

    /// Whether the colour is premultiplied by `alpha` (a `prem` reference
    /// between the two items).
    fn premultiplied(&self, alpha: u32) -> bool {
        self.references_from(b"prem", self.primary).contains(&alpha)
            || self.references_from(b"prem", alpha).contains(&self.primary)
    }

    /// The primary item's coded colour and alpha, and whether the colour is
    /// premultiplied.
    fn primary_items(&self) -> Result<(Coded<'a>, Option<Coded<'a>>, bool)> {
        let alpha = self.alpha_item();
        Ok((
            self.coded(self.primary)?,
            alpha.map(|a| self.coded(a)).transpose()?,
            alpha.is_some_and(|a| self.premultiplied(a)),
        ))
    }
}

/// The primary item's properties.
fn primary_properties(bytes: &[u8]) -> Option<Properties> {
    Meta::parse(bytes)?.primary_properties()
}

/// The crop (x, y, width, height) of a clean aperture on a `width`×`height`
/// image, if it lies on whole pixels inside the image (as libavif requires).
fn clean_aperture(clap: [i64; 8], width: usize, height: usize) -> Option<[usize; 4]> {
    let [wn, wd, hn, hd, xn, xd, yn, yd] = clap.map(i128::from);
    let size = |n: i128, d: i128| (d > 0 && n > 0 && n % d == 0).then(|| n / d);
    let (crop_w, crop_h) = (size(wn, wd)?, size(hn, hd)?);
    // The aperture's centre lies at offset + (full − 1) / 2, so its first
    // pixel at offset + (full − aperture) / 2.
    let edge = |n: i128, d: i128, full: usize, crop: i128| {
        let numerator = 2 * n + (full as i128 - crop) * d;
        (d > 0 && numerator % (2 * d) == 0)
            .then(|| numerator / (2 * d))
            .filter(|&e| e >= 0 && e + crop <= full as i128)
    };
    let x = edge(xn, xd, width, crop_w)?;
    let y = edge(yn, yd, height, crop_h)?;
    Some([x as usize, y as usize, crop_w as usize, crop_h as usize])
}

/// The EXIF orientation that equals rotating `rotation` quarter turns
/// anticlockwise, then mirroring along `mirror`'s axis (HEIF's order).
fn exif_orientation(rotation: u8, mirror: Option<u8>) -> u16 {
    match (rotation & 3, mirror) {
        (0, None) => 1,
        (0, Some(1)) | (2, Some(0)) => 2,
        (2, None) => 3,
        (0, Some(_)) | (2, Some(_)) => 4,
        (1, Some(0)) | (3, Some(1)) => 5,
        (3, None) => 6,
        (1, Some(1)) | (3, Some(_)) => 7,
        _ => 8,
    }
}

/// The orientation (as an EXIF value, 1–8) an AVIF file is displayed with.
pub fn orientation(bytes: &[u8]) -> u16 {
    primary_properties(bytes).map_or(1, |p| exif_orientation(p.rotation, p.mirror))
}

/// One decoded AV1 picture, released with the decoder.
struct Picture(Dav1dPicture);

impl Drop for Picture {
    fn drop(&mut self) {
        // SAFETY: the picture came from dav1d_get_picture and is released once.
        unsafe { dav1d_picture_unref(Some(NonNull::from(&mut self.0))) };
    }
}

/// An AV1 decoder and the spatial layer it outputs (`lsel`; else the last
/// picture, the highest layer).
struct Decoder(Option<Dav1dContext>, Option<u16>);

impl Drop for Decoder {
    fn drop(&mut self) {
        // SAFETY: the context came from dav1d_open; dav1d_close clears it.
        unsafe { dav1d_close(Some(NonNull::from(&mut self.0))) };
    }
}

impl Decoder {
    /// A decoder with `threads` worker threads (0: one per logical core;
    /// 1: decodes on the calling thread) for items of `layering`.
    fn new(threads: i32, layering: Layering) -> Result<Self> {
        let mut settings = MaybeUninit::<Dav1dSettings>::uninit();
        // SAFETY: dav1d_default_settings initialises every field.
        let mut settings = unsafe {
            dav1d_default_settings(NonNull::from(&mut settings).cast());
            settings.assume_init()
        };
        // Still images: no frame pipelining, one picture per item (the
        // highest spatial layer of the operating point, unless `lsel` picks
        // one: then every layer is output and `decode` keeps that one), film
        // grain applied as every AVIF reader does. Above a gigapixel the
        // header is taken for corrupt.
        settings.n_threads = threads;
        settings.max_frame_delay = 1;
        settings.apply_grain = 1;
        settings.operating_point = i32::from(layering.operating_point);
        settings.all_layers = i32::from(layering.layer.is_some());
        settings.frame_size_limit = 1 << 30;
        let mut context = None;
        // SAFETY: both pointers are valid for the call.
        let result = unsafe {
            dav1d_open(
                Some(NonNull::from(&mut context)),
                Some(NonNull::from(&mut settings)),
            )
        };
        if result.0 != 0 || context.is_none() {
            bail!("AV1 decoder failed to start ({})", result.0);
        }
        Ok(Self(context, layering.layer))
    }

    /// Decodes one AVIF item (a single temporal unit) to its last picture,
    /// or to the selected spatial layer's.
    fn decode(&mut self, av1: &[u8]) -> Result<Picture> {
        let context = self.0;
        let layer = self.1;
        let wanted = |picture: &Picture| {
            layer.is_none_or(|layer| {
                // SAFETY: the frame header belongs to the picture.
                picture
                    .0
                    .frame_hdr
                    .is_some_and(|h| u16::from(unsafe { h.as_ref() }.spatial_id) == layer)
            })
        };
        let mut data = Dav1dData::default();
        // SAFETY: dav1d_data_create returns a buffer of `av1.len()` bytes or null.
        let buffer = unsafe { dav1d_data_create(Some(NonNull::from(&mut data)), av1.len()) };
        let Some(buffer) = NonNull::new(buffer) else {
            bail!("AV1 decoder could not allocate {} bytes", av1.len());
        };
        // SAFETY: the buffer holds `av1.len()` bytes and does not overlap `av1`.
        unsafe { std::ptr::copy_nonoverlapping(av1.as_ptr(), buffer.as_ptr(), av1.len()) };

        let mut last: Option<Picture> = None;
        let next_picture = || -> Option<Picture> {
            let mut picture = Dav1dPicture::default();
            // SAFETY: a valid context and an empty picture to fill.
            let result = unsafe { dav1d_get_picture(context, Some(NonNull::from(&mut picture))) };
            (result.0 == 0).then_some(Picture(picture))
        };
        let mut error = None;
        while data.sz > 0 {
            // SAFETY: `data` owns its buffer; the decoder takes what it consumes.
            let result = unsafe { dav1d_send_data(context, Some(NonNull::from(&mut data))) };
            if result.0 == 0 {
                continue;
            }
            // The decoder holds a finished picture (EAGAIN): take it, retry.
            match next_picture() {
                Some(picture) => {
                    if wanted(&picture) {
                        last = Some(picture);
                    }
                }
                None => {
                    error = Some(result.0);
                    break;
                }
            }
        }
        // SAFETY: releases what the decoder did not consume (nothing on success).
        unsafe { dav1d_data_unref(Some(NonNull::from(&mut data))) };
        if let Some(code) = error {
            bail!("AV1 data could not be decoded ({code})");
        }
        // A layered item yields one picture per output layer; the last is
        // the highest.
        while let Some(picture) = next_picture() {
            if wanted(&picture) {
                last = Some(picture);
            }
        }
        match (last, layer) {
            (Some(picture), _) => Ok(picture),
            (None, Some(layer)) => bail!("AV1 data has no spatial layer {layer}"),
            (None, None) => bail!("AV1 data held no picture"),
        }
    }
}

/// One plane of a picture as a row reader.
struct Plane<'a> {
    data: PlaneData<'a>,
    width: usize,
    height: usize,
    /// In samples.
    stride: usize,
}

enum PlaneData<'a> {
    U8(&'a [u8]),
    U16(&'a [u16]),
}

impl Plane<'_> {
    fn row(&self, y: usize, out: &mut [f32]) {
        let start = y.min(self.height - 1) * self.stride;
        match self.data {
            PlaneData::U8(data) => {
                for (o, v) in out.iter_mut().zip(&data[start..start + self.width]) {
                    *o = f32::from(*v);
                }
            }
            PlaneData::U16(data) => {
                for (o, v) in out.iter_mut().zip(&data[start..start + self.width]) {
                    *o = f32::from(*v);
                }
            }
        }
    }
}

struct Frame<'a> {
    width: usize,
    height: usize,
    bits: u32,
    layout: u32,
    planes: Vec<Plane<'a>>,
    /// The sequence header's code points and range.
    cicp: Cicp,
    chroma_position: u32,
}

fn frame(picture: &Picture) -> Result<Frame<'_>> {
    let p = &picture.0;
    let (width, height) = (p.p.w as usize, p.p.h as usize);
    let bits = u32::try_from(p.p.bpc).unwrap_or(0);
    if width == 0 || height == 0 || !matches!(bits, 8 | 10 | 12) {
        bail!("AV1 picture has an unsupported size or bit depth");
    }
    let layout = p.p.layout;
    let (ss_x, ss_y) = match layout {
        DAV1D_PIXEL_LAYOUT_I400 | DAV1D_PIXEL_LAYOUT_I444 => (0, 0),
        DAV1D_PIXEL_LAYOUT_I420 => (1, 1),
        DAV1D_PIXEL_LAYOUT_I422 => (1, 0),
        _ => bail!("AV1 picture has an unknown pixel layout"),
    };
    let plane_count = if layout == DAV1D_PIXEL_LAYOUT_I400 {
        1
    } else {
        3
    };
    let sample_bytes = if bits == 8 { 1 } else { 2 };
    let mut planes = Vec::with_capacity(plane_count);
    for index in 0..plane_count {
        let (w, h) = if index == 0 {
            (width, height)
        } else {
            ((width + ss_x) >> ss_x, (height + ss_y) >> ss_y)
        };
        let stride_bytes = p.stride[usize::from(index > 0)];
        let Some(pointer) = p.data[index] else {
            bail!("AV1 picture lacks a plane");
        };
        if stride_bytes <= 0 || !(stride_bytes as usize).is_multiple_of(sample_bytes) {
            bail!("AV1 picture has an unsupported stride");
        }
        let stride = stride_bytes as usize / sample_bytes;
        if stride < w {
            bail!("AV1 picture has an unsupported stride");
        }
        let len = stride * (h - 1) + w;
        // SAFETY: the decoder allocated `h` rows of `stride` samples for this
        // plane; they live as long as the picture borrowed here.
        let data = unsafe {
            if bits == 8 {
                PlaneData::U8(std::slice::from_raw_parts(
                    pointer.as_ptr().cast::<u8>(),
                    len,
                ))
            } else {
                PlaneData::U16(std::slice::from_raw_parts(
                    pointer.as_ptr().cast::<u16>(),
                    len,
                ))
            }
        };
        planes.push(Plane {
            data,
            width: w,
            height: h,
            stride,
        });
    }
    // SAFETY: the sequence header belongs to the picture.
    let header = p.seq_hdr.map(|h| unsafe { h.as_ref() });
    let cicp = header.map_or(
        Cicp {
            primaries: 2,
            transfer: 2,
            matrix: 2,
            full_range: false,
        },
        |h| Cicp {
            primaries: h.pri as u16,
            transfer: h.trc as u16,
            matrix: h.mtrx as u16,
            full_range: h.color_range != 0,
        },
    );
    Ok(Frame {
        width,
        height,
        bits,
        layout,
        planes,
        cicp,
        chroma_position: header.map_or(0, |h| h.chr),
    })
}

/// CIE xy chromaticities (red, green, blue, white) of H.273 colour primaries.
fn chromaticities(primaries: u16) -> [[f64; 2]; 4] {
    const D65: [f64; 2] = [0.3127, 0.3290];
    match primaries {
        5 => [[0.64, 0.33], [0.29, 0.60], [0.15, 0.06], D65],
        6 | 7 => [[0.630, 0.340], [0.310, 0.595], [0.155, 0.070], D65],
        9 => [[0.708, 0.292], [0.170, 0.797], [0.131, 0.046], D65],
        12 => [[0.680, 0.320], [0.265, 0.690], [0.150, 0.060], D65],
        _ => [[0.64, 0.33], [0.30, 0.60], [0.15, 0.06], D65],
    }
}

/// Luma weights (Kr, Kb) derived from colour primaries (H.273 eq. 39–44).
fn derived_luma_weights(primaries: u16) -> (f64, f64) {
    let [r, g, b, w] = chromaticities(primaries);
    let xyz = |c: [f64; 2]| [c[0] / c[1], 1.0, (1.0 - c[0] - c[1]) / c[1]];
    let (r, g, b, w) = (xyz(r), xyz(g), xyz(b), xyz(w));
    // Solve [r g b]·s = w for the primaries' luminances s.
    let m = [[r[0], g[0], b[0]], [r[1], g[1], b[1]], [r[2], g[2], b[2]]];
    let det = |m: [[f64; 3]; 3]| {
        m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
            - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
            + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
    };
    let d = det(m);
    let solve = |column: usize| {
        let mut n = m;
        for row in 0..3 {
            n[row][column] = w[row];
        }
        det(n) / d
    };
    (solve(0), solve(2))
}

enum Matrix {
    Identity,
    YCgCo,
    YCbCr { kr: f32, kb: f32 },
}

fn matrix(cicp: Cicp) -> Result<Matrix> {
    let weights = |kr: f64, kb: f64| Matrix::YCbCr {
        kr: kr as f32,
        kb: kb as f32,
    };
    Ok(match cicp.matrix {
        0 => Matrix::Identity,
        1 => weights(0.2126, 0.0722),
        4 => weights(0.30, 0.11),
        // BT.601; also "unspecified" and reserved, as other AVIF readers.
        2 | 3 | 5 | 6 => weights(0.299, 0.114),
        7 => weights(0.212, 0.087),
        8 => Matrix::YCgCo,
        9 => weights(0.2627, 0.0593),
        12 => {
            let (kr, kb) = derived_luma_weights(cicp.primaries);
            weights(kr, kb)
        }
        other => bail!("AVIF matrix coefficients {other} are not supported"),
    })
}

/// Chroma upsampling for one axis: the two source samples and the weight of
/// the second for output position `i`.
fn chroma_tap(i: usize, subsampled: bool, co_sited: bool, len: usize) -> (usize, usize, f32) {
    if !subsampled {
        return (i, i, 0.0);
    }
    let c = i / 2;
    let last = len - 1;
    match (co_sited, i.is_multiple_of(2)) {
        // Chroma at the even luma sample.
        (true, true) => (c.min(last), c.min(last), 0.0),
        (true, false) => (c.min(last), (c + 1).min(last), 0.5),
        // Chroma centred between two luma samples: 3:1 bilinear weights.
        (false, true) => (c.saturating_sub(1).min(last), c.min(last), 0.75),
        (false, false) => (c.min(last), (c + 1).min(last), 0.25),
    }
}

/// Converts a frame to RGB in 0…1 (`channels` 3, or 4 with alpha left at 1).
fn to_rgb(frame: &Frame, cicp: Cicp, channels: usize) -> Result<Vec<f32>> {
    let matrix = if frame.layout == DAV1D_PIXEL_LAYOUT_I400 {
        None
    } else {
        Some(matrix(cicp)?)
    };
    if matches!(matrix, Some(Matrix::Identity)) && frame.layout != DAV1D_PIXEL_LAYOUT_I444 {
        bail!("AVIF identity matrix needs 4:4:4 chroma");
    }
    let (width, height) = (frame.width, frame.height);
    let scale = (1u32 << (frame.bits - 8)) as f32;
    let max = ((1u32 << frame.bits) - 1) as f32;
    let (y_offset, y_scale, c_scale) = if cicp.full_range {
        (0.0, 1.0 / max, 1.0 / max)
    } else {
        (16.0 * scale, 1.0 / (219.0 * scale), 1.0 / (224.0 * scale))
    };
    let c_offset = (1u32 << (frame.bits - 1)) as f32;
    // Identity stores R, G, B as full-scale samples (like luma).
    let (c_offset, c_scale) = if matches!(matrix, Some(Matrix::Identity)) {
        (y_offset, y_scale)
    } else {
        (c_offset, c_scale)
    };
    let ss_x = matches!(
        frame.layout,
        DAV1D_PIXEL_LAYOUT_I420 | DAV1D_PIXEL_LAYOUT_I422
    );
    let ss_y = frame.layout == DAV1D_PIXEL_LAYOUT_I420;
    let co_sited_x = matches!(
        frame.chroma_position,
        DAV1D_CHR_VERTICAL | DAV1D_CHR_COLOCATED
    );
    let co_sited_y = frame.chroma_position == DAV1D_CHR_COLOCATED;

    let mut out = Vec::new();
    out.try_reserve_exact(width * height * channels)
        .map_err(|_| anyhow!("AVIF image too large for memory"))?;
    out.resize(width * height * channels, 1.0);
    out.par_chunks_mut(width * channels)
        .enumerate()
        .for_each_init(
            || {
                (
                    vec![0.0f32; width],
                    [vec![0.0f32; width], vec![0.0f32; width]],
                    [vec![0.0f32; width], vec![0.0f32; width]],
                )
            },
            |(luma, u_rows, v_rows), (y, row)| {
                frame.planes[0].row(y, luma);
                let Some(matrix) = &matrix else {
                    for (pixel, l) in row.chunks_exact_mut(channels).zip(luma.iter()) {
                        let v = ((l - y_offset) * y_scale).clamp(0.0, 1.0);
                        pixel[..3].fill(v);
                    }
                    return;
                };
                let (u, v) = (&frame.planes[1], &frame.planes[2]);
                let (ya, yb, wy) = chroma_tap(y, ss_y, co_sited_y, u.height);
                u.row(ya, &mut u_rows[0]);
                u.row(yb, &mut u_rows[1]);
                v.row(ya, &mut v_rows[0]);
                v.row(yb, &mut v_rows[1]);
                for (x, pixel) in row.chunks_exact_mut(channels).enumerate() {
                    let (xa, xb, wx) = chroma_tap(x, ss_x, co_sited_x, u.width);
                    let sample = |rows: &[Vec<f32>; 2]| {
                        let top = rows[0][xa] + (rows[0][xb] - rows[0][xa]) * wx;
                        let bottom = rows[1][xa] + (rows[1][xb] - rows[1][xa]) * wx;
                        top + (bottom - top) * wy
                    };
                    let l = (luma[x] - y_offset) * y_scale;
                    let cb = (sample(u_rows) - c_offset) * c_scale;
                    let cr = (sample(v_rows) - c_offset) * c_scale;
                    let rgb = match *matrix {
                        // GBR: Y carries green, U blue, V red.
                        Matrix::Identity => [cr, l, cb],
                        Matrix::YCgCo => {
                            let t = l - cb;
                            [t + cr, l + cb, t - cr]
                        }
                        Matrix::YCbCr { kr, kb } => {
                            let kg = 1.0 - kr - kb;
                            let r = l + 2.0 * (1.0 - kr) * cr;
                            let b = l + 2.0 * (1.0 - kb) * cb;
                            [r, (l - kr * r - kb * b) / kg, b]
                        }
                    };
                    for (o, c) in pixel.iter_mut().zip(rgb) {
                        *o = c.clamp(0.0, 1.0);
                    }
                }
            },
        );
    Ok(out)
}

/// Alpha in 0…1 of a frame's first plane.
fn alpha_values(frame: &Frame) -> Result<Vec<f32>> {
    let bits = frame.bits;
    let (offset, scale) = if frame.cicp.full_range {
        (0.0, 1.0 / ((1u32 << bits) - 1) as f32)
    } else {
        let s = (1u32 << (bits - 8)) as f32;
        (16.0 * s, 1.0 / (219.0 * s))
    };
    let (width, height) = (frame.width, frame.height);
    let plane = &frame.planes[0];
    let mut out = Vec::new();
    out.try_reserve_exact(width * height)
        .map_err(|_| anyhow!("AVIF image too large for memory"))?;
    out.resize(width * height, 0.0);
    out.par_chunks_mut(width).enumerate().for_each(|(y, row)| {
        plane.row(y, row);
        for a in row {
            *a = ((*a - offset) * scale).clamp(0.0, 1.0);
        }
    });
    Ok(out)
}

/// A decoded item: `channels` values per pixel, its size, and the code
/// points of its (first) AV1 sequence header.
struct Decoded {
    values: Vec<f32>,
    width: usize,
    height: usize,
    cicp: Cicp,
}

impl Decoded {
    fn new(frame: &Frame, values: Vec<f32>) -> Self {
        Self {
            values,
            width: frame.width,
            height: frame.height,
            cicp: frame.cicp,
        }
    }
}

/// Decodes an AV1 item or a grid of them and converts the picture.
fn decode_coded(coded: &Coded, convert: impl Fn(&Frame) -> Result<Vec<f32>>) -> Result<Decoded> {
    match coded {
        Coded::Single(data, layering) => {
            let picture = Decoder::new(0, *layering)?.decode(data)?;
            let frame = frame(&picture)?;
            Ok(Decoded::new(&frame, convert(&frame)?))
        }
        Coded::Grid(grid) => {
            let samples = stitch(grid)?;
            let frame = samples.frame();
            Ok(Decoded::new(&frame, convert(&frame)?))
        }
    }
}

/// A picture's samples, owned (16 bits per sample at every bit depth).
struct Samples {
    width: usize,
    height: usize,
    bits: u32,
    layout: u32,
    cicp: Cicp,
    chroma_position: u32,
    /// (samples, width, height) per plane.
    planes: Vec<(Vec<u16>, usize, usize)>,
}

impl Samples {
    fn of(frame: &Frame) -> Self {
        let planes = frame
            .planes
            .iter()
            .map(|plane| {
                let mut samples = Vec::with_capacity(plane.width * plane.height);
                for y in 0..plane.height {
                    let start = y * plane.stride;
                    match plane.data {
                        PlaneData::U8(data) => samples.extend(
                            data[start..start + plane.width]
                                .iter()
                                .map(|&v| u16::from(v)),
                        ),
                        PlaneData::U16(data) => {
                            samples.extend_from_slice(&data[start..start + plane.width])
                        }
                    }
                }
                (samples, plane.width, plane.height)
            })
            .collect();
        Self {
            width: frame.width,
            height: frame.height,
            bits: frame.bits,
            layout: frame.layout,
            cicp: frame.cicp,
            chroma_position: frame.chroma_position,
            planes,
        }
    }

    fn frame(&self) -> Frame<'_> {
        Frame {
            width: self.width,
            height: self.height,
            bits: self.bits,
            layout: self.layout,
            planes: self
                .planes
                .iter()
                .map(|(data, width, height)| Plane {
                    data: PlaneData::U16(data),
                    width: *width,
                    height: *height,
                    stride: *width,
                })
                .collect(),
            cicp: self.cicp,
            chroma_position: self.chroma_position,
        }
    }
}

/// Reconstructs a grid image: the tiles' YCbCr samples side by side, cut
/// at the grid's size, so the colour conversion (chroma upsampling across
/// tile edges included) runs on the whole image, as for a single item and
/// as libavif does. Tiles decode in parallel, a few rows of them at a time:
/// peak memory is the image's samples plus about one tile per core.
fn stitch(grid: &Grid) -> Result<Samples> {
    let (width, height) = (grid.width, grid.height);
    // Whole rows of tiles, at least one per core, decode side by side;
    // with fewer tiles than cores each decoder takes a share of them.
    let cores = rayon::current_num_threads().max(1);
    let batch = cores.div_ceil(grid.columns) * grid.columns;
    let threads = (cores / batch.min(grid.tiles.len())).clamp(1, 256) as i32;
    let mut out: Option<Samples> = None;
    let mut tile_size = (0, 0);
    for (batch_index, tiles) in grid.tiles.chunks(batch).enumerate() {
        let decoded = tiles
            .par_iter()
            .map_init(
                || Decoder::new(threads, grid.layering),
                |decoder, data| {
                    let decoder = decoder.as_mut().map_err(|e| anyhow!("{e}"))?;
                    let picture = decoder.decode(data)?;
                    Ok(Samples::of(&frame(&picture)?))
                },
            )
            .collect::<Result<Vec<_>>>()?;
        if out.is_none() {
            let first = &decoded[0];
            let (tile_w, tile_h) = (first.width, first.height);
            // Every tile lies at least partly inside the image (MIAF).
            let covers = |tile: usize, count: usize, full: usize| {
                tile * count >= full && tile * (count - 1) < full
            };
            if !covers(tile_w, grid.columns, width) || !covers(tile_h, grid.rows, height) {
                bail!(
                    "AVIF grid tiles of {tile_w}×{tile_h} do not fit a {}×{} grid of {width}×{height}",
                    grid.columns,
                    grid.rows
                );
            }
            let (ss_x, ss_y) = match first.layout {
                DAV1D_PIXEL_LAYOUT_I420 => (1, 1),
                DAV1D_PIXEL_LAYOUT_I422 => (1, 0),
                _ => (0, 0),
            };
            // Subsampled chroma of odd-sized tiles would not line up.
            if (tile_w % 2 == 1 && ss_x == 1) || (tile_h % 2 == 1 && ss_y == 1) {
                bail!("AVIF grid tiles of {tile_w}×{tile_h} do not suit their chroma subsampling");
            }
            let mut planes = Vec::with_capacity(first.planes.len());
            for index in 0..first.planes.len() {
                let (w, h) = if index == 0 {
                    (width, height)
                } else {
                    ((width + ss_x) >> ss_x, (height + ss_y) >> ss_y)
                };
                let mut samples = Vec::new();
                samples
                    .try_reserve_exact(w * h)
                    .map_err(|_| anyhow!("AVIF image too large for memory"))?;
                samples.resize(w * h, 0);
                planes.push((samples, w, h));
            }
            tile_size = (tile_w, tile_h);
            out = Some(Samples {
                width,
                height,
                planes,
                ..*first
            });
        }
        let out = out.as_mut().unwrap();
        let (tile_w, tile_h) = tile_size;
        for (i, tile) in decoded.iter().enumerate() {
            let index = batch_index * batch + i;
            let (row, column) = (index / grid.columns, index % grid.columns);
            let same = (tile.width, tile.height, tile.bits, tile.layout)
                == (tile_w, tile_h, out.bits, out.layout);
            if !same || tile.planes.len() != out.planes.len() {
                bail!("AVIF grid tiles differ in size or format");
            }
            for (index, ((target, w, h), (source, sw, sh))) in
                out.planes.iter_mut().zip(&tile.planes).enumerate()
            {
                // The tile's position in this plane's samples.
                let (left, top) = if index == 0 {
                    (column * tile_w, row * tile_h)
                } else {
                    (column * *sw, row * *sh)
                };
                let copy_w = (*sw).min(*w - left);
                for y in 0..(*sh).min(*h - top) {
                    let at = (top + y) * *w + left;
                    target[at..at + copy_w].copy_from_slice(&source[y * sw..y * sw + copy_w]);
                }
            }
        }
    }
    out.ok_or_else(|| anyhow!("AVIF grid has no tiles"))
}

/// The `[x, y, w, h]` rectangle of `values` (`channels` per pixel, rows of
/// `width`).
fn crop(values: &[f32], width: usize, channels: usize, [x, y, w, h]: [usize; 4]) -> Vec<f32> {
    let mut out = Vec::with_capacity(w * h * channels);
    for row in values.chunks_exact(width * channels).skip(y).take(h) {
        out.extend_from_slice(&row[x * channels..(x + w) * channels]);
    }
    out
}

/// Decodes an AVIF file: the primary item (one AV1 image or a grid of
/// them), its alpha item if any, cropped to the clean aperture, with the
/// colour description and the orientation (not applied).
pub fn decode(bytes: &[u8]) -> Result<AvifImage> {
    let meta = Meta::parse(bytes);
    let properties = meta
        .as_ref()
        .and_then(Meta::primary_properties)
        .unwrap_or_default();
    // avif-parse reads single images; grids are read from the item
    // structure here (avif-parse 2.1 refuses them), and so are the valid
    // single images avif-parse refuses: boxes that run to the end of the
    // file (size 0), items stored in `idat`, an extended `pixi`, the
    // primary item of an image sequence (`avis`).
    let parsed;
    let (color, alpha, premultiplied) = match meta.as_ref().filter(|m| m.is_grid()) {
        Some(meta) => meta.primary_items()?,
        None => match avif_parse::read_avif(&mut &bytes[..]) {
            Ok(read) => {
                parsed = read;
                let layering = |id: Option<u32>| match (&meta, id) {
                    (Some(meta), Some(id)) => meta.layering(id),
                    _ => Ok(Layering::default()),
                };
                let alpha = match parsed.alpha_item.as_deref() {
                    Some(a) => Some(Coded::Single(
                        Cow::Borrowed(a),
                        layering(meta.as_ref().and_then(Meta::alpha_item))?,
                    )),
                    None => None,
                };
                (
                    Coded::Single(
                        Cow::Borrowed(&parsed.primary_item[..]),
                        layering(meta.as_ref().map(|m| m.primary))?,
                    ),
                    alpha,
                    parsed.premultiplied_alpha,
                )
            }
            Err(e) => match meta
                .as_ref()
                .filter(|m| m.kind(m.primary) == Some(*b"av01"))
            {
                Some(meta) => {
                    log::debug!("avif-parse refused the file ({e}); reading its items directly");
                    meta.primary_items()?
                }
                None => {
                    return Err(match e {
                        avif_parse::Error::Unsupported(what) => {
                            anyhow!("AVIF file not supported: {what}")
                        }
                        other => anyhow!("AVIF file could not be read: {other}"),
                    });
                }
            },
        },
    };

    let channels = if alpha.is_some() { 4 } else { 3 };
    let nclx = properties.nclx;
    let decoded = decode_coded(&color, |frame| {
        to_rgb(frame, nclx.unwrap_or(frame.cicp), channels)
    })?;
    drop(color);
    let cicp = nclx.unwrap_or(decoded.cicp);
    let (width, height) = (decoded.width, decoded.height);
    let mut pixels = decoded.values;

    if let Some(alpha) = &alpha {
        let alpha = decode_coded(alpha, alpha_values)?;
        if (alpha.width, alpha.height) != (width, height) {
            bail!("AVIF alpha item does not match the image size");
        }
        pixels
            .par_chunks_mut(width * 4)
            .zip(alpha.values.par_chunks(width))
            .for_each(|(row, values)| {
                for (pixel, &a) in row.as_chunks_mut::<4>().0.iter_mut().zip(values) {
                    if premultiplied && a > 0.0 {
                        for c in &mut pixel[..3] {
                            *c = (*c / a).min(1.0);
                        }
                    }
                    pixel[3] = a;
                }
            });
    }

    // The clean aperture applies before the rotation and mirror (MIAF 7.3.6.7).
    let (mut width, mut height) = (width, height);
    if let Some(clap) = properties.clap {
        match clean_aperture(clap, width, height) {
            Some(rect) if rect != [0, 0, width, height] => {
                pixels = crop(&pixels, width, channels, rect);
                (width, height) = (rect[2], rect[3]);
            }
            Some(_) => {}
            None => log::warn!("AVIF clean aperture {clap:?} is not on whole pixels; not applied"),
        }
    }

    let (w, h) = (width as u32, height as u32);
    let image = if alpha.is_some() {
        DynamicImage::ImageRgba32F(
            ImageBuffer::<Rgba<f32>, _>::from_raw(w, h, pixels)
                .ok_or_else(|| anyhow!("AVIF image buffer size mismatch"))?,
        )
    } else {
        DynamicImage::ImageRgb32F(
            ImageBuffer::<Rgb<f32>, _>::from_raw(w, h, pixels)
                .ok_or_else(|| anyhow!("AVIF image buffer size mismatch"))?,
        )
    };
    Ok(AvifImage {
        image,
        icc: properties.icc,
        cicp,
        orientation: exif_orientation(properties.rotation, properties.mirror),
    })
}

/// Diffuse white of HDR signals, cd/m² (ITU-R BT.2408): 1.0 in the working
/// space, so brighter highlights stay above 1.
pub const HDR_REFERENCE_WHITE: f32 = 203.0;

/// The nominal peak luminance of the HLG reference display (ITU-R BT.2100
/// and BT.2408: 1000 cd/m², system gamma 1.2).
const HLG_PEAK: f32 = 1000.0;
const HLG_GAMMA: f32 = 1.2;

/// SMPTE ST 2084 (PQ) EOTF: signal in 0…1 → display light in cd/m².
fn pq_eotf(signal: f32) -> f32 {
    const M1: f32 = 2610.0 / 16384.0;
    const M2: f32 = 2523.0 / 4096.0 * 128.0;
    const C1: f32 = 3424.0 / 4096.0;
    const C2: f32 = 2413.0 / 4096.0 * 32.0;
    const C3: f32 = 2392.0 / 4096.0 * 32.0;
    let p = signal.clamp(0.0, 1.0).powf(1.0 / M2);
    10_000.0 * ((p - C1).max(0.0) / (C2 - C3 * p)).powf(1.0 / M1)
}

/// ARIB STD-B67 (HLG) inverse OETF: signal in 0…1 → scene light in 0…1.
fn hlg_inverse_oetf(signal: f32) -> f32 {
    const A: f32 = 0.178_832_77;
    const B: f32 = 0.284_668_92;
    const C: f32 = 0.559_910_7;
    let e = signal.clamp(0.0, 1.0);
    if e <= 0.5 {
        e * e / 3.0
    } else {
        (((e - C) / A).exp() + B) / 12.0
    }
}

/// Whether H.273 transfer characteristics are HDR ones (16: PQ, 18: HLG).
fn is_hdr(transfer: u16) -> bool {
    matches!(transfer, 16 | 18)
}

/// H.273 primaries the converters know: unspecified and reserved read as
/// BT.709 (as other AVIF readers display them).
fn known_primaries(primaries: u16) -> u16 {
    if matches!(primaries, 0 | 2 | 3) {
        1
    } else {
        primaries
    }
}

/// PQ or HLG pixels as linear display light relative to diffuse white
/// (`HDR_REFERENCE_WHITE` = 1.0), converted to the working space's
/// primaries and sRGB-encoded without clipping (values above 1 are
/// highlights; the renderer's `srgb_to_linear` inverts the encoding there
/// too). PQ is display light by definition; HLG goes through the BT.2100
/// reference OOTF of a 1000 cd/m² display, so an HLG file and its BT.2408
/// PQ transcode decode to the same values.
fn hdr_to_working_space(image: DynamicImage, cicp: Cicp) -> DynamicImage {
    let primaries = known_primaries(cicp.primaries);
    let matrix = crate::input_profile::primaries_to_working_space(primaries).unwrap_or_else(|| {
        log::warn!("AVIF primaries {primaries} not supported; read as BT.709");
        [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]
    });
    // HLG's OOTF scales by the luminance of the signal's own primaries.
    let (kr, kb) = derived_luma_weights(primaries);
    let (kr, kb) = (kr as f32, kb as f32);
    let kg = 1.0 - kr - kb;
    let pq = cicp.transfer == 16;
    let linearise = move |e: [f32; 3]| -> [f32; 3] {
        if pq {
            return e.map(|v| pq_eotf(v) / HDR_REFERENCE_WHITE);
        }
        let scene = e.map(hlg_inverse_oetf);
        let luminance = kr * scene[0] + kg * scene[1] + kb * scene[2];
        let gain = if luminance > 0.0 {
            HLG_PEAK * luminance.powf(HLG_GAMMA - 1.0) / HDR_REFERENCE_WHITE
        } else {
            0.0
        };
        scene.map(|v| v * gain)
    };
    let convert = move |pixel: &mut [f32]| {
        let linear = linearise([pixel[0], pixel[1], pixel[2]]);
        for (r, out) in matrix.iter().zip(pixel.iter_mut()) {
            let v = r[0] * linear[0] + r[1] * linear[1] + r[2] * linear[2];
            *out = crate::input_profile::encode(v);
        }
    };
    let channels = if image.color().has_alpha() { 4 } else { 3 };
    let (width, height) = (image.width(), image.height());
    let mut values = if channels == 4 {
        image.into_rgba32f().into_raw()
    } else {
        image.into_rgb32f().into_raw()
    };
    values
        .par_chunks_mut(channels * width.max(1) as usize)
        .for_each(|row| row.chunks_exact_mut(channels).for_each(convert));
    if channels == 4 {
        DynamicImage::ImageRgba32F(ImageBuffer::from_raw(width, height, values).unwrap())
    } else {
        DynamicImage::ImageRgb32F(ImageBuffer::from_raw(width, height, values).unwrap())
    }
}

/// The decoded pixels as sRGB-encoded values, as the renderer reads non-RAW
/// sources (input_profile.rs). HDR transfers (PQ, HLG) become linear light
/// with diffuse white at 1.0, encoded without clipping
/// (`hdr_to_working_space`); they take precedence over an ICC profile,
/// which cannot describe them. Otherwise the ICC profile applies when there
/// is one, else the `nclx` primaries and transfer. BT.709 (also
/// unspecified) primaries with an sRGB or BT.709-family transfer are sRGB
/// already. Gain maps (ISO 21496-1 `tmap` items) are not applied: such
/// files show their base image.
pub fn to_working_space(decoded: AvifImage) -> DynamicImage {
    let image = decoded.image;
    if is_hdr(decoded.cicp.transfer) {
        if decoded.icc.is_some() {
            log::debug!("HDR AVIF: the ICC profile is ignored for the PQ/HLG code points");
        }
        return hdr_to_working_space(image, decoded.cicp);
    }
    if let Some(icc) = &decoded.icc {
        return crate::input_profile::to_working_space(image, icc);
    }
    let Cicp {
        primaries,
        transfer,
        ..
    } = decoded.cicp;
    let primaries = known_primaries(primaries);
    let transfer = match transfer {
        // sRGB, unspecified and the BT.709 family read as sRGB, as other
        // AVIF readers display them.
        0..=3 | 6 | 13..=15 => 13,
        other => other,
    };
    if primaries == 1 && transfer == 13 {
        return image;
    }
    let Some(profile) = crate::input_profile::profile_from_cicp(primaries, transfer) else {
        log::warn!(
            "AVIF colour (primaries {primaries}, transfer {transfer}) not supported; read as sRGB"
        );
        return image;
    };
    crate::input_profile::profile_to_working_space(image, &profile)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orientation_codes_match_the_heif_transforms() {
        // A 3×2 image with distinct pixels.
        let source = DynamicImage::ImageRgb8(ImageBuffer::from_fn(3, 2, |x, y| {
            Rgb([x as u8 * 40, y as u8 * 90, 7])
        }));
        for rotation in 0..4u8 {
            for mirror in [None, Some(0), Some(1)] {
                let mut expected = match rotation {
                    0 => source.clone(),
                    1 => source.rotate270(),
                    2 => source.rotate180(),
                    _ => source.rotate90(),
                };
                expected = match mirror {
                    Some(0) => expected.flipv(),
                    Some(_) => expected.fliph(),
                    None => expected,
                };
                let code = exif_orientation(rotation, mirror);
                let actual = crate::image_processing::apply_orientation(
                    source.clone(),
                    rawler::Orientation::from_u16(code),
                );
                assert_eq!(actual, expected, "irot {rotation} imir {mirror:?} → {code}");
            }
        }
    }

    #[test]
    fn recognises_avif_brands() {
        let ftyp = |major: &[u8; 4], compat: &[&[u8; 4]]| {
            let mut b = Vec::new();
            b.extend_from_slice(&((16 + 4 * compat.len()) as u32).to_be_bytes());
            b.extend_from_slice(b"ftyp");
            b.extend_from_slice(major);
            b.extend_from_slice(&[0; 4]);
            for c in compat {
                b.extend_from_slice(*c);
            }
            b
        };
        assert!(is_avif(&ftyp(b"avif", &[b"mif1"])));
        assert!(is_avif(&ftyp(b"mif1", &[b"miaf", b"avif"])));
        assert!(!is_avif(&ftyp(b"heic", &[b"mif1", b"heic"])));
        assert!(!is_avif(b"\x89PNG\r\n\x1a\n"));
    }

    #[test]
    fn derived_luma_weights_match_the_named_matrices() {
        let (kr, kb) = derived_luma_weights(1);
        assert!(
            (kr - 0.2126).abs() < 1e-3 && (kb - 0.0722).abs() < 1e-3,
            "{kr} {kb}"
        );
        let (kr, kb) = derived_luma_weights(9);
        assert!(
            (kr - 0.2627).abs() < 1e-3 && (kb - 0.0593).abs() < 1e-3,
            "{kr} {kb}"
        );
    }

    // Test files: AV1 data from rav1e (frames filled with YCbCr computed
    // here, forward, from a known RGB image) in a minimal HEIF container
    // written below, with the properties under test.

    use image::ImageEncoder as _;
    use rav1e::color::{
        ChromaSampling, ColorDescription, ColorPrimaries, MatrixCoefficients, PixelRange,
        TransferCharacteristics,
    };

    const W: usize = 64;
    const H: usize = 48;

    /// A smooth, saturated RGB test image in 0…1.
    fn source(x: usize, y: usize) -> [f64; 3] {
        let (u, v) = (x as f64 / (W - 1) as f64, y as f64 / (H - 1) as f64);
        [0.1 + 0.8 * u, 0.15 + 0.7 * v, 0.9 - 0.6 * u * v]
    }

    struct Av1 {
        data: Vec<u8>,
        config: Vec<u8>,
    }

    /// YCbCr of an RGB colour for (Kr, Kb), or (monochrome) its luminance.
    fn ycc([r, g, b]: [f64; 3], matrix: Option<(f64, f64)>) -> [f64; 3] {
        let Some((kr, kb)) = matrix else {
            return [0.2126 * r + 0.7152 * g + 0.0722 * b, 0.0, 0.0];
        };
        let l = kr * r + (1.0 - kr - kb) * g + kb * b;
        [
            l,
            (b - l) / (2.0 * (1.0 - kb)),
            (r - l) / (2.0 * (1.0 - kr)),
        ]
    }

    fn description(matrix: Option<(f64, f64)>) -> Option<ColorDescription> {
        matrix.map(|(kr, _)| ColorDescription {
            color_primaries: ColorPrimaries::BT709,
            transfer_characteristics: TransferCharacteristics::SRGB,
            matrix_coefficients: if kr == 0.2126 {
                MatrixCoefficients::BT709
            } else if kr == 0.2627 {
                MatrixCoefficients::BT2020NCL
            } else {
                MatrixCoefficients::BT601
            },
        })
    }

    /// Encodes the source image; `matrix` (Kr, Kb) or monochrome.
    fn encode(
        bits: usize,
        sampling: ChromaSampling,
        full: bool,
        matrix: Option<(f64, f64)>,
    ) -> Av1 {
        encode_planes(
            (W, H),
            bits,
            sampling,
            full,
            description(matrix),
            |plane, x, y| ycc(source(x, y), matrix)[plane],
        )
    }

    /// Encodes a `width`×`height` frame whose planes hold `value(plane, x,
    /// y)`: luma in 0…1 and chroma in −0.5…0.5, or (identity matrix) G, B
    /// and R in 0…1. Chroma samples take the mean over the block they cover.
    fn encode_planes(
        (width, height): (usize, usize),
        bits: usize,
        sampling: ChromaSampling,
        full: bool,
        description: Option<ColorDescription>,
        value: impl Fn(usize, usize, usize) -> f64,
    ) -> Av1 {
        let identity =
            description.is_some_and(|d| d.matrix_coefficients == MatrixCoefficients::Identity);
        let max = ((1u32 << bits) - 1) as f64;
        let k = (1u32 << (bits - 8)) as f64;
        let quantise = |v: f64, chroma: bool| -> u16 {
            let q = match (full, chroma) {
                (true, false) => v * max,
                (true, true) => v * max + (1u32 << (bits - 1)) as f64,
                (false, false) => 16.0 * k + 219.0 * k * v,
                (false, true) => 128.0 * k + 224.0 * k * v,
            };
            q.round().clamp(0.0, max) as u16
        };
        let (sx, sy) = match sampling {
            ChromaSampling::Cs420 => (2, 2),
            ChromaSampling::Cs422 => (2, 1),
            _ => (1, 1),
        };
        let mut config = rav1e::EncoderConfig::with_speed_preset(10);
        config.width = width;
        config.height = height;
        config.bit_depth = bits;
        config.chroma_sampling = sampling;
        config.pixel_range = if full {
            PixelRange::Full
        } else {
            PixelRange::Limited
        };
        config.color_description = description;
        config.still_picture = true;
        config.quantizer = 10;
        config.min_quantizer = 0;
        let mut context: rav1e::Context<u16> = rav1e::Config::new()
            .with_encoder_config(config)
            .new_context()
            .unwrap();
        let mut frame = context.new_frame();
        let planes = if sampling == ChromaSampling::Cs400 {
            1
        } else {
            3
        };
        for (index, plane) in frame.planes.iter_mut().enumerate().take(planes) {
            let (dx, dy) = if index == 0 { (1, 1) } else { (sx, sy) };
            let mut slice = plane.mut_slice(Default::default());
            for (py, row) in slice.rows_iter_mut().take(height.div_ceil(dy)).enumerate() {
                for (px, out) in row.iter_mut().take(width.div_ceil(dx)).enumerate() {
                    let mut sum = 0.0;
                    let mut n = 0.0;
                    for y in py * dy..((py + 1) * dy).min(height) {
                        for x in px * dx..((px + 1) * dx).min(width) {
                            sum += value(index, x, y);
                            n += 1.0;
                        }
                    }
                    *out = quantise(sum / n, index > 0 && !identity);
                }
            }
        }
        context.send_frame(frame).unwrap();
        context.flush();
        let mut data = Vec::new();
        loop {
            match context.receive_packet() {
                Ok(mut packet) => data.append(&mut packet.data),
                Err(rav1e::EncoderStatus::Encoded) => {}
                Err(rav1e::EncoderStatus::LimitReached) => break,
                Err(e) => panic!("{e:?}"),
            }
        }
        Av1 {
            data,
            config: context.container_sequence_header(),
        }
    }

    fn put_box(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut b = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
        b.extend_from_slice(kind);
        b.extend_from_slice(payload);
        b
    }

    fn full_box(kind: &[u8; 4], version: u8, payload: &[u8]) -> Vec<u8> {
        let mut body = vec![version, 0, 0, 0];
        body.extend_from_slice(payload);
        put_box(kind, &body)
    }

    fn nclx(primaries: u16, transfer: u16, matrix: u16) -> Vec<u8> {
        let mut colr = b"nclx".to_vec();
        for v in [primaries, transfer, matrix] {
            colr.extend_from_slice(&v.to_be_bytes());
        }
        colr.push(0x80);
        put_box(b"colr", &colr)
    }

    fn icc(profile: &[u8]) -> Vec<u8> {
        put_box(b"colr", &[b"prof".as_slice(), profile].concat())
    }

    fn ispe(width: usize, height: usize) -> Vec<u8> {
        let mut size = (width as u32).to_be_bytes().to_vec();
        size.extend_from_slice(&(height as u32).to_be_bytes());
        full_box(b"ispe", 0, &size)
    }

    /// A clean aperture: width, height, horizontal and vertical offset as
    /// (numerator, denominator).
    fn clap(fields: [(i32, u32); 4]) -> Vec<u8> {
        let mut payload = Vec::new();
        for (n, d) in fields {
            payload.extend_from_slice(&n.to_be_bytes());
            payload.extend_from_slice(&d.to_be_bytes());
        }
        put_box(b"clap", &payload)
    }

    struct Item {
        id: u16,
        kind: [u8; 4],
        data: Vec<u8>,
        /// In `idat` (construction method 1) instead of `mdat`.
        in_idat: bool,
        properties: Vec<Vec<u8>>,
    }

    /// An AV1 item with ispe, av1C and `extra` properties.
    fn av01(id: u16, av1: &Av1, (width, height): (usize, usize), extra: &[Vec<u8>]) -> Item {
        let mut properties = vec![ispe(width, height), put_box(b"av1C", &av1.config)];
        properties.extend(extra.iter().cloned());
        Item {
            id,
            kind: *b"av01",
            data: av1.data.clone(),
            in_idat: false,
            properties,
        }
    }

    /// A HEIF file of `items` (the first is the primary item) and
    /// `references` (type, from, to).
    fn container(items: &[Item], references: &[(&[u8; 4], u16, Vec<u16>)]) -> Vec<u8> {
        let ftyp = put_box(b"ftyp", b"avif\0\0\0\0avifmif1miaf");
        let all: Vec<u8> = items.iter().flat_map(|i| i.properties.concat()).collect();
        let mut ipma = (items.len() as u32).to_be_bytes().to_vec();
        let mut index = 0u8;
        for item in items {
            ipma.extend_from_slice(&item.id.to_be_bytes());
            ipma.push(item.properties.len() as u8);
            for property in &item.properties {
                index += 1;
                // av1C is essential.
                let essential = &property[4..8] == b"av1C";
                ipma.push(if essential { 0x80 | index } else { index });
            }
        }
        let iprp = put_box(
            b"iprp",
            &[put_box(b"ipco", &all), full_box(b"ipma", 0, &ipma)].concat(),
        );
        let mut entries = (items.len() as u16).to_be_bytes().to_vec();
        for item in items {
            let mut infe = item.id.to_be_bytes().to_vec();
            infe.extend_from_slice(&[0, 0]);
            infe.extend_from_slice(&item.kind);
            infe.push(0);
            entries.extend(full_box(b"infe", 2, &infe));
        }
        let iinf = full_box(b"iinf", 0, &entries);
        let iref = if references.is_empty() {
            Vec::new()
        } else {
            let mut boxes = Vec::new();
            for (kind, from, to) in references {
                let mut reference = from.to_be_bytes().to_vec();
                reference.extend_from_slice(&(to.len() as u16).to_be_bytes());
                for t in to {
                    reference.extend_from_slice(&t.to_be_bytes());
                }
                boxes.extend(put_box(kind, &reference));
            }
            full_box(b"iref", 0, &boxes)
        };
        let stored = |idat: bool| -> Vec<u8> {
            items
                .iter()
                .filter(|i| i.in_idat == idat)
                .flat_map(|i| i.data.clone())
                .collect()
        };
        let idat = match stored(true) {
            data if data.is_empty() => Vec::new(),
            data => put_box(b"idat", &data),
        };
        let hdlr = full_box(b"hdlr", 0, b"\0\0\0\0pict\0\0\0\0\0\0\0\0\0\0\0\0\0");
        let pitm = full_box(b"pitm", 0, &items[0].id.to_be_bytes());
        let meta = |mdat_start: u32| {
            // Version 1: 4-byte offsets and lengths, no base offset, one
            // extent per item, construction method 0 (file) or 1 (idat).
            let mut iloc = vec![0x44, 0x00];
            iloc.extend_from_slice(&(items.len() as u16).to_be_bytes());
            let (mut in_file, mut in_idat) = (mdat_start, 0u32);
            for item in items {
                iloc.extend_from_slice(&item.id.to_be_bytes());
                iloc.extend_from_slice(&u16::from(item.in_idat).to_be_bytes());
                iloc.extend_from_slice(&[0, 0, 0, 1]);
                let at = if item.in_idat {
                    &mut in_idat
                } else {
                    &mut in_file
                };
                iloc.extend_from_slice(&at.to_be_bytes());
                iloc.extend_from_slice(&(item.data.len() as u32).to_be_bytes());
                *at += item.data.len() as u32;
            }
            full_box(
                b"meta",
                0,
                &[
                    hdlr.clone(),
                    pitm.clone(),
                    full_box(b"iloc", 1, &iloc),
                    iinf.clone(),
                    iref.clone(),
                    iprp.clone(),
                    idat.clone(),
                ]
                .concat(),
            )
        };
        let offset = (ftyp.len() + meta(0).len() + 8) as u32;
        [ftyp, meta(offset), put_box(b"mdat", &stored(false))].concat()
    }

    /// A one-item HEIF file: `av1` as item 1 with the properties ispe,
    /// av1C and `extra`.
    fn heif(av1: &Av1, extra: &[Vec<u8>]) -> Vec<u8> {
        container(&[av01(1, av1, (W, H), extra)], &[])
    }

    /// Mean and maximum difference to the source image (or its luminance).
    fn error(image: &DynamicImage, grey: bool) -> (f64, f64) {
        let rgb = image.to_rgb32f();
        let mut sum = 0.0;
        let mut max = 0.0f64;
        for (x, y, p) in rgb.enumerate_pixels() {
            let s = source(x as usize, y as usize);
            let s = if grey {
                [0.2126 * s[0] + 0.7152 * s[1] + 0.0722 * s[2]; 3]
            } else {
                s
            };
            for c in 0..3 {
                let d = (f64::from(p[c]) - s[c]).abs();
                sum += d;
                max = max.max(d);
            }
        }
        (sum / (W * H * 3) as f64, max)
    }

    #[test]
    fn decodes_every_layout_depth_range_and_matrix() {
        let cases = [
            (8, ChromaSampling::Cs420, true, Some((0.299, 0.114))),
            (8, ChromaSampling::Cs444, false, Some((0.2126, 0.0722))),
            (10, ChromaSampling::Cs422, false, Some((0.2126, 0.0722))),
            (10, ChromaSampling::Cs420, true, Some((0.2627, 0.0593))),
            (12, ChromaSampling::Cs444, true, Some((0.2627, 0.0593))),
            (12, ChromaSampling::Cs420, false, Some((0.299, 0.114))),
            (8, ChromaSampling::Cs400, true, None),
            (10, ChromaSampling::Cs400, false, None),
        ];
        for (bits, sampling, full, matrix) in cases {
            let bytes = heif(&encode(bits, sampling, full, matrix), &[]);
            assert!(is_avif(&bytes));
            let decoded = decode(&bytes).unwrap();
            assert_eq!(decoded.image.color(), image::ColorType::Rgb32F);
            assert_eq!((decoded.image.width(), decoded.image.height()), (64, 48));
            let (mean, max) = error(&decoded.image, matrix.is_none());
            assert!(
                mean < 1.5 / 255.0 && max < 8.0 / 255.0,
                "{bits}-bit {sampling:?} full {full} {matrix:?}: mean {:.2}, max {:.2} levels",
                mean * 255.0,
                max * 255.0
            );
        }
    }

    #[test]
    fn applies_rotation_and_mirror() {
        let av1 = encode(8, ChromaSampling::Cs444, true, Some((0.299, 0.114)));
        let plain = decode(&heif(&av1, &[])).unwrap();
        let bytes = heif(&av1, &[put_box(b"irot", &[1]), put_box(b"imir", &[1])]);
        assert_eq!(orientation(&bytes), 7);
        assert_eq!(decode(&bytes).unwrap().orientation, 7);
        let loaded = crate::image_loader::load_image_with_orientation(&bytes, None).unwrap();
        // A quarter turn anticlockwise, then left and right exchanged.
        let expected = plain.image.rotate270().fliph();
        assert_eq!(loaded.to_rgb32f(), expected.to_rgb32f());
        assert_eq!(crate::image_loader::source_orientation(&bytes, "x.avif"), 7);
    }

    #[test]
    fn reads_colour_from_icc_or_nclx() {
        use crate::output_color_space::OutputColorSpace;
        let av1 = encode(10, ChromaSampling::Cs444, true, Some((0.299, 0.114)));
        let load = |extra: &[Vec<u8>]| {
            crate::image_loader::load_image_with_orientation(&heif(&av1, extra), None)
                .unwrap()
                .into_rgb32f()
        };
        let raw = decode(&heif(&av1, &[])).unwrap().image;
        let p3_icc = crate::icc_profile::for_space(OutputColorSpace::DisplayP3);
        let expected = crate::input_profile::to_working_space(raw.clone(), p3_icc).into_rgb32f();
        let close = |a: &image::Rgb32FImage, b: &image::Rgb32FImage, tolerance: f32| {
            a.as_raw()
                .iter()
                .zip(b.as_raw())
                .all(|(x, y)| (x - y).abs() < tolerance)
        };
        // sRGB code points, or none: the values as decoded.
        assert_eq!(load(&[nclx(1, 13, 6)]), raw.to_rgb32f());
        assert_eq!(load(&[]), raw.to_rgb32f());
        // An ICC profile, also after an nclx box that says sRGB.
        assert_eq!(load(&[icc(p3_icc)]), expected);
        assert_eq!(load(&[nclx(1, 13, 6), icc(p3_icc)]), expected);
        // Display P3 code points convert like the Display P3 profile.
        let from_nclx = load(&[nclx(12, 13, 6)]);
        assert!(!close(&from_nclx, &raw.to_rgb32f(), 1e-3));
        assert!(close(&from_nclx, &expected, 2e-3));
    }

    #[test]
    fn decodes_alpha() {
        let rgba = image::RgbaImage::from_fn(W as u32, H as u32, |x, y| {
            let [r, g, b] = source(x as usize, y as usize).map(|v| (v * 255.0).round() as u8);
            image::Rgba([r, g, b, (x * 4) as u8])
        });
        let mut bytes = Vec::new();
        image::codecs::avif::AvifEncoder::new_with_speed_quality(&mut bytes, 10, 95)
            .write_image(
                rgba.as_raw(),
                W as u32,
                H as u32,
                image::ExtendedColorType::Rgba8,
            )
            .unwrap();
        let decoded = decode(&bytes).unwrap();
        assert_eq!(decoded.image.color(), image::ColorType::Rgba32F);
        let out = decoded.image.to_rgba32f();
        for x in [0, 20, 63] {
            let a = out.get_pixel(x, 10)[3];
            assert!(
                (a - (x * 4) as f32 / 255.0).abs() < 3.0 / 255.0,
                "x {x}: {a}"
            );
        }
    }

    #[test]
    fn applies_the_clean_aperture() {
        let av1 = encode(8, ChromaSampling::Cs444, true, Some((0.299, 0.114)));
        let plain = decode(&heif(&av1, &[])).unwrap().image;
        // 40×30, centred 4 pixels right of and 2 above the image centre.
        let aperture = clap([(40, 1), (30, 1), (4, 1), (-2, 1)]);
        let expected = plain.crop_imm(16, 7, 40, 30);
        let cropped = decode(&heif(&av1, std::slice::from_ref(&aperture))).unwrap();
        assert_eq!(cropped.image, expected);
        // The same aperture in other fractions.
        let halves = clap([(80, 2), (60, 2), (8, 2), (-4, 2)]);
        assert_eq!(decode(&heif(&av1, &[halves])).unwrap().image, expected);
        // The crop applies before the rotation.
        let bytes = heif(&av1, &[aperture, put_box(b"irot", &[1])]);
        let loaded = crate::image_loader::load_image_with_orientation(&bytes, None).unwrap();
        assert_eq!(loaded.to_rgb32f(), expected.rotate270().to_rgb32f());
        // Off whole pixels, or outside the image: not applied.
        for fields in [
            [(40, 1), (30, 1), (1, 2), (0, 1)],
            [(40, 1), (30, 1), (20, 1), (0, 1)],
            [(65, 1), (30, 1), (0, 1), (0, 1)],
            [(40, 1), (30, 1), (0, 1), (0, 0)],
        ] {
            let image = decode(&heif(&av1, &[clap(fields)])).unwrap().image;
            assert_eq!(image, plain, "{fields:?}");
        }
    }

    const GW: usize = 100;
    const GH: usize = 70;
    const TILE: usize = 40;

    /// The test image of the grid files, 100×70.
    fn grid_source(x: usize, y: usize) -> [f64; 3] {
        let (u, v) = (x as f64 / (GW - 1) as f64, y as f64 / (GH - 1) as f64);
        [0.1 + 0.8 * u, 0.15 + 0.7 * v, 0.9 - 0.6 * u * v]
    }

    fn grid_alpha(x: usize) -> f64 {
        x as f64 / (GW - 1) as f64
    }

    /// A grid item: rows × columns of tiles cropped to width × height
    /// (descriptor in `idat`, as libavif writes it).
    fn grid_item(
        id: u16,
        (rows, columns): (u8, u8),
        (width, height): (usize, usize),
        properties: Vec<Vec<u8>>,
    ) -> Item {
        let mut data = vec![0, 0, rows - 1, columns - 1];
        data.extend_from_slice(&(width as u16).to_be_bytes());
        data.extend_from_slice(&(height as u16).to_be_bytes());
        Item {
            id,
            kind: *b"grid",
            data,
            in_idat: true,
            properties,
        }
    }

    /// The 100×70 grid source as 2×3 tiles of 40×40 (cut at the right and
    /// bottom), with an alpha grid if `alpha`; BT.601 `nclx` on the grid
    /// item or on the tiles; `extra` properties on the grid item.
    fn grid_file(alpha: bool, colour_on_tiles: bool, extra: &[Vec<u8>]) -> Vec<u8> {
        let matrix = Some((0.299, 0.114));
        let colour = nclx(1, 13, 6);
        let tile = |id: u16, index: usize, monochrome: bool| {
            let (left, top) = ((index % 3) * TILE, (index / 3) * TILE);
            // Pixels beyond the image repeat its edge.
            let at = |x: usize, y: usize| ((left + x).min(GW - 1), (top + y).min(GH - 1));
            let av1 = if monochrome {
                encode_planes(
                    (TILE, TILE),
                    8,
                    ChromaSampling::Cs400,
                    true,
                    None,
                    |_, x, y| grid_alpha(at(x, y).0),
                )
            } else {
                encode_planes(
                    (TILE, TILE),
                    8,
                    ChromaSampling::Cs420,
                    true,
                    description(matrix),
                    |plane, x, y| {
                        let (x, y) = at(x, y);
                        ycc(grid_source(x, y), matrix)[plane]
                    },
                )
            };
            let extra = if colour_on_tiles && !monochrome {
                vec![colour.clone()]
            } else {
                Vec::new()
            };
            av01(id, &av1, (TILE, TILE), &extra)
        };
        let mut properties = vec![ispe(GW, GH)];
        if !colour_on_tiles {
            properties.push(colour.clone());
        }
        properties.extend(extra.iter().cloned());
        let mut items = vec![grid_item(1, (2, 3), (GW, GH), properties)];
        items.extend((0..6).map(|i| tile(2 + i as u16, i, false)));
        let mut references = vec![(b"dimg", 1, (2..8).collect())];
        if alpha {
            let aux = full_box(b"auxC", 0, b"urn:mpeg:mpegB:cicp:systems:auxiliary:alpha\0");
            items.push(grid_item(8, (2, 3), (GW, GH), vec![ispe(GW, GH), aux]));
            items.extend((0..6).map(|i| tile(9 + i as u16, i, true)));
            references.push((b"dimg", 8, (9..15).collect()));
            references.push((b"auxl", 8, vec![1]));
        }
        container(&items, &references)
    }

    #[test]
    fn decodes_grid_images() {
        for alpha in [false, true] {
            let decoded = decode(&grid_file(alpha, false, &[])).unwrap();
            let image = &decoded.image;
            assert_eq!((image.width(), image.height()), (GW as u32, GH as u32));
            assert_eq!(image.color().has_alpha(), alpha);
            assert_eq!(decoded.cicp.matrix, 6);
            let rgba = image.to_rgba32f();
            let (mut sum, mut max) = (0.0, 0.0f64);
            for (x, y, p) in rgba.enumerate_pixels() {
                let s = grid_source(x as usize, y as usize);
                for c in 0..3 {
                    let d = (f64::from(p[c]) - s[c]).abs();
                    sum += d;
                    max = max.max(d);
                }
                if alpha {
                    let a = grid_alpha(x as usize);
                    assert!((f64::from(p[3]) - a).abs() < 3.0 / 255.0, "{x},{y}: {p:?}");
                }
            }
            let mean = sum / (GW * GH * 3) as f64;
            assert!(
                mean < 1.5 / 255.0 && max < 8.0 / 255.0,
                "alpha {alpha}: mean {:.2}, max {:.2} levels",
                mean * 255.0,
                max * 255.0
            );
        }
        // On two cores the tiles decode one grid row at a time: the same image.
        let bytes = grid_file(true, false, &[]);
        let two_cores = rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .unwrap()
            .install(|| decode(&bytes).unwrap());
        assert_eq!(two_cores.image, decode(&bytes).unwrap().image);
    }

    #[test]
    fn grids_take_colour_orientation_and_crop_from_the_grid_item() {
        let plain = decode(&grid_file(false, false, &[])).unwrap();
        let bt601 = Cicp {
            primaries: 1,
            transfer: 13,
            matrix: 6,
            full_range: true,
        };
        assert_eq!(plain.cicp, bt601);
        // Colour only on the tiles: the first tile's.
        let on_tiles = decode(&grid_file(false, true, &[])).unwrap();
        assert_eq!(on_tiles.cicp, bt601);
        assert_eq!(on_tiles.image, plain.image);
        // A quarter turn clockwise.
        let bytes = grid_file(false, false, &[put_box(b"irot", &[3])]);
        assert_eq!(orientation(&bytes), 6);
        let loaded = crate::image_loader::load_image_with_orientation(&bytes, None).unwrap();
        assert_eq!(loaded.to_rgb32f(), plain.image.rotate90().to_rgb32f());
        // 60×50 at (21, 9).
        let aperture = clap([(60, 1), (50, 1), (1, 1), (-1, 1)]);
        let cropped = decode(&grid_file(false, false, &[aperture])).unwrap();
        assert_eq!(cropped.image, plain.image.crop_imm(21, 9, 60, 50));
    }

    #[test]
    fn broken_grids_fail_clearly() {
        let av1 = encode_planes(
            (TILE, TILE),
            8,
            ChromaSampling::Cs444,
            true,
            None,
            |_, _, _| 0.5,
        );
        let tile = || av01(2, &av1, (TILE, TILE), &[]);
        let error = |bytes: Vec<u8>| decode(&bytes).err().unwrap().to_string();
        // A 2×2 grid listing one tile.
        let bytes = container(
            &[grid_item(1, (2, 2), (60, 60), vec![]), tile()],
            &[(b"dimg", 1, vec![2])],
        );
        assert!(error(bytes).contains("2×2 lists 1 tiles"));
        // One 40×40 tile for a 100×70 image.
        let bytes = container(
            &[grid_item(1, (1, 1), (GW, GH), vec![]), tile()],
            &[(b"dimg", 1, vec![2])],
        );
        assert!(error(bytes).contains("do not fit"));
        // A tile that is not AV1.
        let mut other = tile();
        other.kind = *b"hvc1";
        let bytes = container(
            &[grid_item(1, (1, 1), (TILE, TILE), vec![]), other],
            &[(b"dimg", 1, vec![2])],
        );
        assert!(error(bytes).contains("not an AV1 image"));
        // One tile in two cells.
        let bytes = container(
            &[grid_item(1, (1, 2), (2 * TILE, TILE), vec![]), tile()],
            &[(b"dimg", 1, vec![2, 2])],
        );
        assert!(error(bytes).contains("more than once"));
        assert!(crate::formats::is_supported_image_file("photo.AVIF"));
    }

    #[test]
    fn reads_single_images_avif_parse_refuses() {
        let av1 = encode(8, ChromaSampling::Cs420, true, Some((0.299, 0.114)));
        let plain = decode(&heif(&av1, &[])).unwrap().image.to_rgb32f();
        let refused = |bytes: &[u8]| avif_parse::read_avif(&mut &bytes[..]).is_err();
        let check = |bytes: &[u8], what: &str| {
            let decoded = decode(bytes).unwrap_or_else(|e| panic!("{what}: {e:#}"));
            assert_eq!(decoded.image.to_rgb32f(), plain, "{what}");
        };
        // The item stored in `idat` (construction method 1).
        let mut item = av01(1, &av1, (W, H), &[]);
        item.in_idat = true;
        let in_idat = container(&[item], &[]);
        assert!(refused(&in_idat));
        check(&in_idat, "idat");
        // `mdat` running to the end of the file (size 0).
        let mut open_ended = heif(&av1, &[]);
        let at = open_ended.len() - av1.data.len() - 8;
        assert_eq!(&open_ended[at + 4..at + 8], b"mdat");
        open_ended[at..at + 4].fill(0);
        assert!(refused(&open_ended));
        check(&open_ended, "mdat of size 0");
        // The primary item of an image sequence.
        let mut sequence = heif(&av1, &[]);
        sequence[8..12].copy_from_slice(b"avis");
        assert!(refused(&sequence));
        check(&sequence, "avis");
        // Without a readable item structure, avif-parse's reason.
        let error = decode(&sequence[..40]).err().unwrap().to_string();
        assert!(error.contains("AVIF file not supported"), "{error}");
    }

    #[test]
    fn selects_the_operating_point_and_layer() {
        let av1 = encode(8, ChromaSampling::Cs444, true, Some((0.299, 0.114)));
        let bytes = heif(&av1, &[put_box(b"a1op", &[2]), put_box(b"lsel", &[0, 1])]);
        let meta = Meta::parse(&bytes).unwrap();
        let layering = Layering {
            operating_point: 2,
            layer: Some(1),
        };
        assert_eq!(meta.properties(1).unwrap().layering, layering);
        let plain = decode(&heif(&av1, &[])).unwrap().image.to_rgb32f();
        // The stream's one layer: by operating point 0, by its spatial ID
        // or as the highest (0xFFFF).
        for extra in [
            vec![put_box(b"a1op", &[0])],
            vec![put_box(b"lsel", &[0, 0])],
            vec![put_box(b"a1op", &[0]), put_box(b"lsel", &[0xFF, 0xFF])],
        ] {
            assert_eq!(
                decode(&heif(&av1, &extra)).unwrap().image.to_rgb32f(),
                plain
            );
        }
        let error = |extra: Vec<u8>| decode(&heif(&av1, &[extra])).err().unwrap().to_string();
        assert!(error(put_box(b"lsel", &[0, 1])).contains("no spatial layer 1"));
        // AV1 has spatial layers 0–3.
        assert!(error(put_box(b"lsel", &[0, 4])).contains("not valid"));
    }

    // HDR test files: BT.2020 colours of known linear light (relative to
    // diffuse white, 203 cd/m²) in 16×16 patches, encoded through the
    // transfer function here (independently of the decoder's), as 12-bit
    // 4:4:4 RGB (identity matrix).

    const PQ_PATCHES: [[f64; 3]; 12] = [
        [0.0; 3],
        [0.01; 3],
        [0.1; 3],
        [0.18; 3],
        [0.5; 3],
        [1.0; 3],
        [2.0; 3],
        [5.0; 3],
        [20.0; 3],
        [49.0; 3],
        [2.0, 0.5, 0.1],
        [0.05, 0.8, 0.3],
    ];

    /// Up to the HLG reference display's peak (1000 / 203 = 4.93).
    const HLG_PATCHES: [[f64; 3]; 12] = [
        [0.0; 3],
        [0.01; 3],
        [0.1; 3],
        [0.18; 3],
        [0.5; 3],
        [1.0; 3],
        [2.0; 3],
        [3.0; 3],
        [4.5; 3],
        [4.9; 3],
        [2.0, 0.5, 0.1],
        [0.05, 0.8, 0.3],
    ];

    /// ST 2084 inverse EOTF of cd/m².
    fn pq_signal(nits: f64) -> f64 {
        let (m1, m2) = (2610.0 / 16384.0, 2523.0 / 4096.0 * 128.0);
        let (c1, c2, c3) = (
            3424.0 / 4096.0,
            2413.0 / 4096.0 * 32.0,
            2392.0 / 4096.0 * 32.0,
        );
        let y = (nits / 10_000.0).powf(m1);
        ((c1 + c2 * y) / (1.0 + c3 * y)).powf(m2)
    }

    /// HLG signal of display light relative to diffuse white: the BT.2100
    /// OOTF (1000 cd/m², gamma 1.2) inverted, then the OETF.
    fn hlg_signal(display: [f64; 3]) -> [f64; 3] {
        let nits = display.map(|v| v * 203.0);
        let luminance = 0.2627 * nits[0] + 0.6780 * nits[1] + 0.0593 * nits[2];
        let scene_luminance = (luminance / 1000.0).powf(1.0 / 1.2);
        let scene = nits.map(|v| {
            if v == 0.0 {
                0.0
            } else {
                v / (1000.0 * scene_luminance.powf(0.2))
            }
        });
        let (a, b, c) = (0.17883277, 0.28466892, 0.55991073);
        scene.map(|e| {
            assert!(e <= 1.0, "{display:?} is beyond the HLG range");
            if e <= 1.0 / 12.0 {
                (3.0 * e).sqrt()
            } else {
                a * (12.0 * e - b).ln() + c
            }
        })
    }

    fn hdr_file(transfer: u16, signal: impl Fn(usize) -> [f64; 3]) -> Vec<u8> {
        let description = ColorDescription {
            color_primaries: ColorPrimaries::BT2020,
            transfer_characteristics: if transfer == 16 {
                TransferCharacteristics::SMPTE2084
            } else {
                TransferCharacteristics::HLG
            },
            matrix_coefficients: MatrixCoefficients::Identity,
        };
        let signals: Vec<[f64; 3]> = (0..12).map(signal).collect();
        // Planes G, B, R.
        let av1 = encode_planes(
            (W, H),
            12,
            ChromaSampling::Cs444,
            true,
            Some(description),
            |plane, x, y| signals[(y / 16) * 4 + x / 16][[1, 2, 0][plane]],
        );
        heif(&av1, &[nclx(9, transfer, 0)])
    }

    /// The loaded patch centres as linear BT.709 light.
    fn loaded_patches(bytes: &[u8]) -> Vec<[f64; 3]> {
        let loaded = crate::image_loader::load_image_with_orientation(bytes, None)
            .unwrap()
            .into_rgb32f();
        (0..12)
            .map(|i| {
                let p = loaded.get_pixel((i % 4 * 16 + 8) as u32, (i / 4 * 16 + 8) as u32);
                p.0.map(|v| {
                    let v = f64::from(v);
                    if v <= 0.04045 {
                        v / 12.92
                    } else {
                        ((v + 0.055) / 1.055).powf(2.4)
                    }
                })
            })
            .collect()
    }

    /// BT.2020 → BT.709 linear light (ITU-R BT.2087).
    fn bt2020_to_bt709(c: [f64; 3]) -> [f64; 3] {
        const M: [[f64; 3]; 3] = [
            [1.6605, -0.5876, -0.0728],
            [-0.1246, 1.1329, -0.0083],
            [-0.0182, -0.1006, 1.1187],
        ];
        M.map(|r| r[0] * c[0] + r[1] * c[1] + r[2] * c[2])
    }

    fn assert_patches(name: &str, patches: &[[f64; 3]; 12], bytes: &[u8]) {
        for (patch, actual) in patches.iter().zip(loaded_patches(bytes)) {
            let expected = bt2020_to_bt709(*patch);
            eprintln!("DBG {name} {patch:?} {actual:?} {expected:?}");
            for c in 0..3 {
                let tolerance = 0.01 * expected[c].abs().max(patch[c]) + 1e-3;
                assert!(
                    (actual[c] - expected[c]).abs() < tolerance,
                    "{name} {patch:?}: {actual:?}, expected {expected:?}"
                );
            }
        }
    }

    #[test]
    fn pq_decodes_to_linear_light_with_diffuse_white_at_one() {
        let bytes = hdr_file(16, |i| PQ_PATCHES[i].map(|v| pq_signal(v * 203.0)));
        assert_patches("PQ", &PQ_PATCHES, &bytes);
    }

    #[test]
    fn hlg_decodes_through_the_reference_ootf() {
        let bytes = hdr_file(18, |i| hlg_signal(HLG_PATCHES[i]));
        assert_patches("HLG", &HLG_PATCHES, &bytes);
    }

    #[test]
    fn hdr_reference_white_is_one() {
        let bt2100 = |transfer| Cicp {
            primaries: 9,
            transfer,
            matrix: 9,
            full_range: true,
        };
        // BT.2408: 203 cd/m² in PQ, 75 % HLG.
        for (transfer, signal) in [(16, pq_signal(203.0) as f32), (18, 0.75)] {
            let image = DynamicImage::ImageRgb32F(ImageBuffer::from_pixel(1, 1, Rgb([signal; 3])));
            let out = hdr_to_working_space(image, bt2100(transfer)).into_rgb32f();
            for v in out.get_pixel(0, 0).0 {
                assert!((v - 1.0).abs() < 2e-3, "transfer {transfer}: {v}");
            }
        }
        // Peaks stay above 1: 10000 cd/m² PQ is 49 times diffuse white.
        let image = DynamicImage::ImageRgb32F(ImageBuffer::from_pixel(1, 1, Rgb([1.0; 3])));
        let out = hdr_to_working_space(image, bt2100(16)).into_rgb32f();
        let linear = ((out.get_pixel(0, 0)[1] + 0.055) / 1.055).powf(2.4);
        assert!((linear - 10_000.0 / 203.0).abs() < 0.05, "{linear}");
    }

    /// Every `.avif` below `AVIF_SAMPLES` (e.g. libavif's tests/data and the
    /// AOM av1-avif testFiles; local only, never committed): decode time,
    /// size, code points, or why it is refused. With `AVIF_REFERENCE`, a
    /// directory of `avifdec -d 16` PNGs named by the path relative to
    /// `AVIF_SAMPLES` with `/` as `__`, the oriented and cropped pixels are
    /// compared (encoded values in 0…1: max and mean absolute difference of
    /// colour, then of alpha). Reports only; it asserts nothing.
    #[test]
    #[ignore = "requires AVIF_SAMPLES pointing to a local directory of sample AVIFs"]
    fn sample_files() {
        fn walk(dir: &std::path::Path, files: &mut Vec<std::path::PathBuf>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    walk(&path, files);
                } else if path.extension().is_some_and(|e| e == "avif") {
                    files.push(path);
                }
            }
        }
        let root = std::path::PathBuf::from(std::env::var("AVIF_SAMPLES").expect("AVIF_SAMPLES"));
        let reference = std::env::var("AVIF_REFERENCE")
            .ok()
            .map(std::path::PathBuf::from);
        let mut files = Vec::new();
        walk(&root, &mut files);
        files.sort();
        for path in files {
            let name = path
                .strip_prefix(&root)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            let bytes = std::fs::read(&path).unwrap();
            let start = std::time::Instant::now();
            let decoded = decode(&bytes);
            let ms = start.elapsed().as_secs_f64() * 1000.0;
            let decoded = match decoded {
                Ok(decoded) => decoded,
                Err(e) => {
                    println!("REFUSED {name}: {e:#}");
                    continue;
                }
            };
            let c = decoded.cicp;
            let summary = format!(
                "{}×{} {}ch cicp {}/{}/{}{} icc {} orient {} {ms:.1} ms",
                decoded.image.width(),
                decoded.image.height(),
                decoded.image.color().channel_count(),
                c.primaries,
                c.transfer,
                c.matrix,
                if c.full_range { " full" } else { " limited" },
                decoded.icc.is_some(),
                decoded.orientation,
            );
            let alpha = decoded.image.color().has_alpha();
            let oriented = crate::image_processing::apply_orientation(
                decoded.image,
                rawler::Orientation::from_u16(decoded.orientation),
            )
            .into_rgba32f();
            let reference_path = reference.as_ref().map(|dir| {
                dir.join(format!(
                    "{}.png",
                    name.trim_end_matches(".avif").replace('/', "__")
                ))
            });
            let Some(Ok(expected)) = reference_path.map(image::open) else {
                println!("DECODED {name}: {summary} (no reference)");
                // For a look: `AVIF_OUT`, 8-bit PNGs of the files without one.
                if let Ok(out) = std::env::var("AVIF_OUT") {
                    let png =
                        std::path::Path::new(&out).join(format!("{}.png", name.replace('/', "__")));
                    DynamicImage::ImageRgba32F(oriented)
                        .to_rgba8()
                        .save(png)
                        .unwrap();
                }
                continue;
            };
            let expected = expected.into_rgba32f();
            if expected.dimensions() != oriented.dimensions() {
                println!(
                    "MISMATCH {name}: {summary}; reference {:?}",
                    expected.dimensions()
                );
                continue;
            }
            let (mut colour, mut alpha_diff) = ((0.0f32, 0.0f64), (0.0f32, 0.0f64));
            for (a, b) in oriented.pixels().zip(expected.pixels()) {
                for i in 0..3 {
                    let d = (a[i] - b[i]).abs();
                    colour.0 = colour.0.max(d);
                    colour.1 += f64::from(d);
                }
                let d = (a[3] - b[3]).abs();
                alpha_diff.0 = alpha_diff.0.max(d);
                alpha_diff.1 += f64::from(d);
            }
            let n = f64::from(oriented.width() * oriented.height());
            println!(
                "DECODED {name}: {summary}; vs avifdec colour max {:.4} mean {:.5}{}",
                colour.0,
                colour.1 / n / 3.0,
                if alpha {
                    format!(
                        ", alpha max {:.4} mean {:.5}",
                        alpha_diff.0,
                        alpha_diff.1 / n
                    )
                } else {
                    String::new()
                }
            );
        }
    }
}
