//! ICC profile and XMP for exported AVIF files.
//!
//! The AVIF encoder (ravif via image) writes the colour description only into
//! the AV1 sequence header, fixed to sRGB, and has no XMP item. This rebuilds
//! the HEIF `meta` box: an ICC `colr` property (`prof`) associated with the
//! primary item, and XMP as a `mime` item (`application/rdf+xml`) that
//! describes the primary item (`cdsc` reference), its bytes appended to the
//! media data. Item locations are rewritten for the moved media data. ISO/IEC
//! 23008-12 (HEIF) and the AVIF specification; no third-party code.

struct Box<'a> {
    kind: [u8; 4],
    /// The whole box, header included.
    bytes: &'a [u8],
    /// Where the payload starts within `bytes`.
    header: usize,
}

impl<'a> Box<'a> {
    fn payload(&self) -> &'a [u8] {
        &self.bytes[self.header..]
    }
}

fn boxes(data: &[u8]) -> Result<Vec<Box<'_>>, String> {
    let mut out = Vec::new();
    let mut at = 0;
    while at + 8 <= data.len() {
        let size32 = u32::from_be_bytes(data[at..at + 4].try_into().unwrap()) as usize;
        let kind: [u8; 4] = data[at + 4..at + 8].try_into().unwrap();
        let (size, header) = match size32 {
            0 => (data.len() - at, 8),
            1 => {
                let large = data
                    .get(at + 8..at + 16)
                    .ok_or("AVIF box header is truncated")?;
                (u64::from_be_bytes(large.try_into().unwrap()) as usize, 16)
            }
            n => (n, 8),
        };
        if size < header || at + size > data.len() {
            return Err(format!(
                "AVIF box {} is truncated",
                String::from_utf8_lossy(&kind)
            ));
        }
        out.push(Box {
            kind,
            bytes: &data[at..at + size],
            header,
        });
        at += size;
    }
    if at != data.len() {
        return Err("AVIF file ends inside a box header".to_string());
    }
    Ok(out)
}

fn write_box(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 8);
    out.extend_from_slice(&(payload.len() as u32 + 8).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(payload);
    out
}

fn full_box(kind: &[u8; 4], version: u8, flags: u32, payload: &[u8]) -> Vec<u8> {
    let mut body = Vec::with_capacity(payload.len() + 4);
    body.push(version);
    body.extend_from_slice(&flags.to_be_bytes()[1..]);
    body.extend_from_slice(payload);
    write_box(kind, &body)
}

struct Reader<'a> {
    data: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn uint(&mut self, bytes: usize) -> Result<u64, String> {
        let slice = self
            .data
            .get(self.at..self.at + bytes)
            .ok_or("AVIF item table is truncated")?;
        self.at += bytes;
        Ok(slice.iter().fold(0u64, |v, b| (v << 8) | u64::from(*b)))
    }
}

struct Extent {
    offset: u64,
    length: u64,
}

struct Location {
    item: u32,
    construction_method: u8,
    data_reference: u16,
    extents: Vec<Extent>,
}

fn parse_iloc(full: &[u8]) -> Result<(u8, Vec<Location>), String> {
    let version = full[0];
    let mut r = Reader {
        data: &full[4..],
        at: 0,
    };
    let sizes = r.uint(2)? as u16;
    let offset_size = (sizes >> 12) as usize;
    let length_size = ((sizes >> 8) & 15) as usize;
    let base_offset_size = ((sizes >> 4) & 15) as usize;
    let index_size = if version >= 1 {
        (sizes & 15) as usize
    } else {
        0
    };
    let count = r.uint(if version < 2 { 2 } else { 4 })?;
    let mut items = Vec::new();
    for _ in 0..count {
        let item = r.uint(if version < 2 { 2 } else { 4 })? as u32;
        let construction_method = if version >= 1 {
            (r.uint(2)? & 15) as u8
        } else {
            0
        };
        let data_reference = r.uint(2)? as u16;
        let base = r.uint(base_offset_size)?;
        let extent_count = r.uint(2)?;
        let mut extents = Vec::new();
        for _ in 0..extent_count {
            if index_size > 0 {
                r.uint(index_size)?;
            }
            let offset = base + r.uint(offset_size)?;
            let length = r.uint(length_size)?;
            extents.push(Extent { offset, length });
        }
        items.push(Location {
            item,
            construction_method,
            data_reference,
            extents,
        });
    }
    Ok((version, items))
}

fn write_iloc(items: &[Location]) -> Vec<u8> {
    let version: u8 = if items.iter().any(|l| l.construction_method != 0) {
        1
    } else {
        0
    };
    let wide_ids = items.iter().any(|l| l.item > u16::MAX as u32);
    let version = if wide_ids { 2 } else { version };
    let mut body = Vec::new();
    // Offsets and lengths in 4 bytes, no base offset, no extent index.
    body.extend_from_slice(&0x4400u16.to_be_bytes());
    if version < 2 {
        body.extend_from_slice(&(items.len() as u16).to_be_bytes());
    } else {
        body.extend_from_slice(&(items.len() as u32).to_be_bytes());
    }
    for l in items {
        if version < 2 {
            body.extend_from_slice(&(l.item as u16).to_be_bytes());
        } else {
            body.extend_from_slice(&l.item.to_be_bytes());
        }
        if version >= 1 {
            body.extend_from_slice(&u16::from(l.construction_method).to_be_bytes());
        }
        body.extend_from_slice(&l.data_reference.to_be_bytes());
        body.extend_from_slice(&(l.extents.len() as u16).to_be_bytes());
        for e in &l.extents {
            body.extend_from_slice(&(e.offset as u32).to_be_bytes());
            body.extend_from_slice(&(e.length as u32).to_be_bytes());
        }
    }
    full_box(b"iloc", version, 0, &body)
}

struct Association {
    item: u32,
    /// (essential, property index from 1).
    properties: Vec<(bool, u16)>,
}

fn parse_ipma(full: &[u8]) -> Result<(u8, u32, Vec<Association>), String> {
    let version = full[0];
    let flags = u32::from_be_bytes([0, full[1], full[2], full[3]]);
    let mut r = Reader {
        data: &full[4..],
        at: 0,
    };
    let count = r.uint(4)?;
    let mut out = Vec::new();
    for _ in 0..count {
        let item = r.uint(if version < 1 { 2 } else { 4 })? as u32;
        let n = r.uint(1)?;
        let mut properties = Vec::new();
        for _ in 0..n {
            if flags & 1 != 0 {
                let v = r.uint(2)? as u16;
                properties.push((v & 0x8000 != 0, v & 0x7fff));
            } else {
                let v = r.uint(1)? as u16;
                properties.push((v & 0x80 != 0, v & 0x7f));
            }
        }
        out.push(Association { item, properties });
    }
    Ok((version, flags, out))
}

fn write_ipma(version: u8, flags: u32, associations: &[Association]) -> Vec<u8> {
    let wide = flags & 1 != 0
        || associations
            .iter()
            .flat_map(|a| &a.properties)
            .any(|(_, i)| *i > 0x7f);
    let flags = if wide { flags | 1 } else { flags };
    let mut body = Vec::new();
    body.extend_from_slice(&(associations.len() as u32).to_be_bytes());
    for a in associations {
        if version < 1 {
            body.extend_from_slice(&(a.item as u16).to_be_bytes());
        } else {
            body.extend_from_slice(&a.item.to_be_bytes());
        }
        body.push(a.properties.len() as u8);
        for (essential, index) in &a.properties {
            if wide {
                body.extend_from_slice(
                    &(index | if *essential { 0x8000 } else { 0 }).to_be_bytes(),
                );
            } else {
                body.push(*index as u8 | if *essential { 0x80 } else { 0 });
            }
        }
    }
    full_box(b"ipma", version, flags, &body)
}

/// Adds an ICC profile (`colr`/`prof` property of the primary item) and an
/// XMP packet to an AVIF file written by the encoder.
pub fn set_metadata(
    avif: &[u8],
    icc: Option<&[u8]>,
    xmp: Option<&[u8]>,
) -> Result<Vec<u8>, String> {
    if icc.is_none() && xmp.is_none() {
        return Ok(avif.to_vec());
    }
    let top = boxes(avif)?;
    let meta = top
        .iter()
        .find(|b| &b.kind == b"meta")
        .ok_or("AVIF file has no meta box")?;
    let mdat_index = top
        .iter()
        .position(|b| &b.kind == b"mdat")
        .ok_or("AVIF file has no mdat box")?;
    let meta_full = meta.payload();
    let children = boxes(&meta_full[4..])?;

    let primary = {
        let pitm = children
            .iter()
            .find(|b| &b.kind == b"pitm")
            .ok_or("AVIF file has no primary item")?
            .payload();
        if pitm[0] == 0 {
            u32::from(u16::from_be_bytes([pitm[4], pitm[5]]))
        } else {
            u32::from_be_bytes(pitm[4..8].try_into().unwrap())
        }
    };
    let (_, mut locations) = parse_iloc(
        children
            .iter()
            .find(|b| &b.kind == b"iloc")
            .ok_or("AVIF file has no item locations")?
            .payload(),
    )?;
    let iinf = children
        .iter()
        .find(|b| &b.kind == b"iinf")
        .ok_or("AVIF file has no item info")?;
    let iinf_full = iinf.payload();
    let entry_bytes = if iinf_full[0] == 0 { 2 } else { 4 };
    let infe_entries = &iinf_full[4 + entry_bytes..];
    let mut entry_count = Reader {
        data: &iinf_full[4..],
        at: 0,
    }
    .uint(entry_bytes)? as u32;
    let next_item = locations.iter().map(|l| l.item).max().unwrap_or(0) + 1;

    // Item properties: the colr box appended to ipco, associated with the primary item.
    let iprp = children
        .iter()
        .find(|b| &b.kind == b"iprp")
        .ok_or("AVIF file has no item properties")?;
    let iprp_children = boxes(iprp.payload())?;
    let ipco = iprp_children
        .iter()
        .find(|b| &b.kind == b"ipco")
        .ok_or("AVIF file has no property container")?;
    let ipma = iprp_children
        .iter()
        .find(|b| &b.kind == b"ipma")
        .ok_or("AVIF file has no property associations")?;
    let mut properties = ipco.payload().to_vec();
    let (ipma_version, ipma_flags, mut associations) = parse_ipma(ipma.payload())?;
    if let Some(icc) = icc {
        let index = boxes(ipco.payload())?.len() as u16 + 1;
        let mut colr = b"prof".to_vec();
        colr.extend_from_slice(icc);
        properties.extend(write_box(b"colr", &colr));
        let entry = associations
            .iter_mut()
            .find(|a| a.item == primary)
            .ok_or("AVIF primary item has no properties")?;
        // Colour properties come before transformative ones (none here).
        entry.properties.push((false, index));
    }
    let mut iprp_out = Vec::new();
    for child in &iprp_children {
        match &child.kind {
            b"ipco" => iprp_out.extend(write_box(b"ipco", &properties)),
            b"ipma" => iprp_out.extend(write_ipma(ipma_version, ipma_flags, &associations)),
            _ => iprp_out.extend_from_slice(child.bytes),
        }
    }

    // XMP: a mime item described by the primary item.
    let mut infe_out = infe_entries.to_vec();
    let mut iref_extra = Vec::new();
    if xmp.is_some() {
        let mut infe = Vec::new();
        if next_item > u16::MAX as u32 {
            return Err("AVIF file has too many items".to_string());
        }
        infe.extend_from_slice(&(next_item as u16).to_be_bytes());
        infe.extend_from_slice(&0u16.to_be_bytes()); // protection index
        infe.extend_from_slice(b"mime");
        infe.extend_from_slice(b"XMP\0");
        infe.extend_from_slice(b"application/rdf+xml\0");
        infe_out.extend(full_box(b"infe", 2, 0, &infe));
        entry_count += 1;
        let mut reference = Vec::new();
        reference.extend_from_slice(&(next_item as u16).to_be_bytes());
        reference.extend_from_slice(&1u16.to_be_bytes());
        reference.extend_from_slice(&(primary as u16).to_be_bytes());
        iref_extra = write_box(b"cdsc", &reference);
    }
    let mut iinf_body = vec![iinf_full[0], iinf_full[1], iinf_full[2], iinf_full[3]];
    if entry_bytes == 2 {
        iinf_body.extend_from_slice(&(entry_count as u16).to_be_bytes());
    } else {
        iinf_body.extend_from_slice(&entry_count.to_be_bytes());
    }
    iinf_body.extend_from_slice(&infe_out);
    let iinf_out = write_box(b"iinf", &iinf_body);
    let has_iref = children.iter().any(|b| &b.kind == b"iref");
    let iref_out = |existing: Option<&Box>| -> Vec<u8> {
        match existing {
            Some(b) => {
                let mut body = b.payload().to_vec();
                body.extend_from_slice(&iref_extra);
                write_box(b"iref", &body)
            }
            None => full_box(b"iref", 0, 0, &iref_extra),
        }
    };

    // Lay the file out once with placeholder locations to learn where the
    // media data moves, then again with the real ones (same sizes).
    let old_mdat = &top[mdat_index];
    let old_payload_start = top[..mdat_index]
        .iter()
        .map(|b| b.bytes.len())
        .sum::<usize>()
        + old_mdat.header;
    let build = |locations: &[Location]| -> Vec<u8> {
        let mut meta_body = vec![meta_full[0], meta_full[1], meta_full[2], meta_full[3]];
        for child in &children {
            match &child.kind {
                b"iloc" => meta_body.extend(write_iloc(locations)),
                b"iinf" => meta_body.extend_from_slice(&iinf_out),
                b"iprp" => meta_body.extend(write_box(b"iprp", &iprp_out)),
                b"iref" if !iref_extra.is_empty() => meta_body.extend(iref_out(Some(child))),
                _ => meta_body.extend_from_slice(child.bytes),
            }
        }
        if !iref_extra.is_empty() && !has_iref {
            meta_body.extend(iref_out(None));
        }
        write_box(b"meta", &meta_body)
    };
    if xmp.is_some() {
        locations.push(Location {
            item: next_item,
            construction_method: 0,
            data_reference: 0,
            extents: vec![Extent {
                offset: 0,
                length: xmp.map_or(0, |x| x.len() as u64),
            }],
        });
    }
    let new_meta_len = build(&locations).len();
    let mut out = Vec::with_capacity(avif.len() + new_meta_len);
    let mut new_payload_start = 0;
    for (i, b) in top.iter().enumerate() {
        if &b.kind == b"meta" {
            out.resize(out.len() + new_meta_len, 0);
        } else if i == mdat_index {
            new_payload_start = out.len() + 8;
            let mut payload = b.payload().to_vec();
            if let Some(xmp) = xmp {
                payload.extend_from_slice(xmp);
            }
            out.extend(write_box(b"mdat", &payload));
        } else {
            out.extend_from_slice(b.bytes);
        }
    }
    let shift =
        |offset: u64| -> u64 { offset + new_payload_start as u64 - old_payload_start as u64 };
    let payload_len = old_mdat.payload().len() as u64;
    for l in &mut locations {
        if l.construction_method != 0 || l.data_reference != 0 {
            continue;
        }
        for e in &mut l.extents {
            if l.item == next_item && xmp.is_some() {
                e.offset = (new_payload_start as u64) + payload_len;
            } else {
                e.offset = shift(e.offset);
            }
        }
    }
    let meta_bytes = build(&locations);
    debug_assert_eq!(meta_bytes.len(), new_meta_len);
    let meta_at = top
        .iter()
        .take_while(|b| &b.kind != b"meta")
        .map(|b| b.bytes.len())
        .sum::<usize>();
    out[meta_at..meta_at + new_meta_len].copy_from_slice(&meta_bytes);
    if out.len() > u32::MAX as usize {
        return Err("AVIF file is too large for 32-bit item offsets".to_string());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{DynamicImage, ImageEncoder, RgbImage};

    fn encoded() -> Vec<u8> {
        let image = DynamicImage::ImageRgb8(RgbImage::from_fn(32, 16, |x, y| {
            image::Rgb([(x * 8) as u8, (y * 16) as u8, 90])
        }));
        let mut bytes = Vec::new();
        let encoder = image::codecs::avif::AvifEncoder::new_with_speed_quality(&mut bytes, 10, 80);
        encoder
            .write_image(image.as_bytes(), 32, 16, image::ExtendedColorType::Rgb8)
            .unwrap();
        bytes
    }

    fn meta_children(avif: &[u8]) -> Vec<([u8; 4], Vec<u8>)> {
        let top = boxes(avif).unwrap();
        let meta = top.iter().find(|b| &b.kind == b"meta").unwrap();
        boxes(&meta.payload()[4..])
            .unwrap()
            .iter()
            .map(|b| (b.kind, b.payload().to_vec()))
            .collect()
    }

    fn primary_data(avif: &[u8]) -> Vec<u8> {
        let children = meta_children(avif);
        let pitm = &children.iter().find(|(k, _)| k == b"pitm").unwrap().1;
        let primary = u32::from(u16::from_be_bytes([pitm[4], pitm[5]]));
        let iloc = &children.iter().find(|(k, _)| k == b"iloc").unwrap().1;
        let (_, items) = parse_iloc(iloc).unwrap();
        let l = items.iter().find(|l| l.item == primary).unwrap();
        l.extents
            .iter()
            .flat_map(|e| avif[e.offset as usize..(e.offset + e.length) as usize].to_vec())
            .collect()
    }

    #[test]
    fn adds_profile_and_xmp_and_keeps_the_image_data() {
        let avif = encoded();
        let icc =
            crate::icc_profile::for_space(crate::output_color_space::OutputColorSpace::DisplayP3);
        let xmp = b"<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"/>";
        let out = set_metadata(&avif, Some(icc), Some(xmp)).unwrap();
        assert_eq!(primary_data(&avif), primary_data(&out));
        let children = meta_children(&out);
        let iprp = &children.iter().find(|(k, _)| k == b"iprp").unwrap().1;
        let iprp_children = boxes(iprp).unwrap();
        let ipco = boxes(iprp_children[0].payload()).unwrap();
        let colr = ipco.iter().find(|b| &b.kind == b"colr").unwrap();
        assert_eq!(&colr.payload()[..4], b"prof");
        assert_eq!(&colr.payload()[4..], icc);
        // The XMP item's data is where its location says.
        let (_, items) =
            parse_iloc(&children.iter().find(|(k, _)| k == b"iloc").unwrap().1).unwrap();
        let x = items.iter().max_by_key(|l| l.item).unwrap();
        let e = &x.extents[0];
        assert_eq!(&out[e.offset as usize..(e.offset + e.length) as usize], xmp);
        assert!(children.iter().any(|(k, _)| k == b"iref"));
    }

    #[test]
    fn nothing_to_add_leaves_the_file_alone() {
        let avif = encoded();
        assert_eq!(set_metadata(&avif, None, None).unwrap(), avif);
    }
}
