//! A 3D colour look-up table as the GPU pipeline samples it (`size`³ RGB
//! entries, optionally after a 1D shaper) and its parsers for .cube, .3dl
//! and HaldCLUT images. The app's `lut_processing` adds Android content
//! URIs and the LUT library.
use anyhow::anyhow;
use image::{DynamicImage, GenericImageView};
use std::fs::File;
use std::io::{BufRead, BufReader, Cursor};
use std::path::Path;

#[derive(Debug, Clone, PartialEq)]
pub struct Lut {
    pub size: u32,
    pub data: Vec<f32>,
    /// The stage a colour passes before the table (a .cube's 1D shaper or
    /// input range); `None`: the table spans the unit cube.
    pub shaper: Option<Shaper>,
}

/// Refuses network, device and traversal paths for LUT files.
pub fn validate_lut_path(path_str: &str) -> anyhow::Result<()> {
    let normalized_path_str = path_str.strip_prefix(r"\\?\").unwrap_or(path_str);

    if normalized_path_str.starts_with(r"\\") || normalized_path_str.starts_with("//") {
        return Err(anyhow!("Network paths (UNC) are not allowed for LUTs"));
    }

    // A `..` component, not two dots inside a name (`Tone..v2.cube`).
    let path = std::path::Path::new(path_str);
    if path
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(anyhow!("Directory traversal (..) is not allowed"));
    }

    if let Some(std::path::Component::Prefix(prefix)) = path.components().next() {
        match prefix.kind() {
            std::path::Prefix::UNC(_, _)
            | std::path::Prefix::VerbatimUNC(_, _)
            | std::path::Prefix::DeviceNS(_) => {
                return Err(anyhow!("Device/UNC prefix paths are not allowed"));
            }
            _ => {}
        }
    }

    Ok(())
}

pub fn parse_lut_file(path_str: &str) -> anyhow::Result<Lut> {
    validate_lut_path(path_str)?;
    let extension = Path::new(path_str)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_lowercase();
    match extension.as_str() {
        "cube" => parse_cube(std::fs::read(path_str)?),
        "3dl" => {
            let file = File::open(path_str)?;
            parse_3dl(BufReader::new(file))
        }
        "png" | "jpg" | "jpeg" | "tiff" => parse_hald(image::open(path_str)?),
        _ => Err(anyhow!("Unsupported LUT file format: {}", extension)),
    }
}

/// A LUT read into memory (Android content URIs), by its file extension.
pub fn parse_lut_bytes(extension: &str, bytes: Vec<u8>) -> anyhow::Result<Lut> {
    match extension {
        "cube" => parse_cube(bytes),
        "3dl" => parse_3dl(BufReader::new(Cursor::new(bytes))),
        "png" | "jpg" | "jpeg" | "tiff" => parse_hald(image::load_from_memory(&bytes)?),
        _ => Err(anyhow!("Unsupported LUT file format: {}", extension)),
    }
}

/// Reads a .cube file. A file the earlier reader took (a 3D table:
/// `LUT_3D_SIZE` and its data, other lines ignored) is read exactly as it
/// was, so saved edits render as before, `DOMAIN_MIN`/`DOMAIN_MAX`
/// included (ignored by that reader); anything else goes to
/// [`parse_cube_full`] (1D tables, shapers, domains).
fn parse_cube(bytes: Vec<u8>) -> anyhow::Result<Lut> {
    parse_cube_3d(BufReader::new(Cursor::new(&bytes)))
        .or_else(|_| parse_cube_full(BufReader::new(Cursor::new(&bytes))))
}

/// A LUT to import, by its file extension: as [`parse_lut_bytes`], but a
/// .cube that the earlier reader takes differently from the full reading
/// (a 3D table with a domain other than 0–1, which that reader ignores) is
/// refused, so no new edit depends on that reading.
pub fn parse_lut_bytes_for_import(extension: &str, bytes: Vec<u8>) -> anyhow::Result<Lut> {
    if extension != "cube" {
        return parse_lut_bytes(extension, bytes);
    }
    match (
        parse_cube_3d(BufReader::new(Cursor::new(&bytes))),
        parse_cube_full(BufReader::new(Cursor::new(&bytes))),
    ) {
        (Ok(earlier), Ok(full)) if earlier == full => Ok(earlier),
        (Ok(_), _) => Err(anyhow!(
            "A 3D table with a domain other than 0–1 (DOMAIN_MIN/MAX or LUT_3D_INPUT_RANGE) is not supported"
        )),
        (Err(_), full) => full,
    }
}

/// The 3D .cube reader before 1D tables, shapers and domains (unchanged).
fn parse_cube_3d(reader: impl BufRead) -> anyhow::Result<Lut> {
    let mut size: Option<u32> = None;
    let mut data: Vec<f32> = Vec::new();
    let mut line_num = 0;

    for line in reader.lines() {
        line_num += 1;
        let line = line?;
        let trimmed = line.trim();

        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let parts: Vec<&str> = trimmed.split_whitespace().collect();
        if parts.is_empty() {
            continue;
        }

        match parts[0].to_uppercase().as_str() {
            "TITLE" | "DOMAIN_MIN" | "DOMAIN_MAX" => continue,

            "LUT_3D_SIZE" => {
                if parts.len() < 2 {
                    return Err(anyhow!(
                        "Malformed LUT_3D_SIZE on line {}: '{}'",
                        line_num,
                        line
                    ));
                }
                size = Some(parts[1].parse().map_err(|e| {
                    anyhow!(
                        "Failed to parse LUT_3D_SIZE on line {}: '{}'. Error: {}",
                        line_num,
                        line,
                        e
                    )
                })?);
            }
            _ => {
                if size.is_some() {
                    if parts.len() < 3 {
                        return Err(anyhow!(
                            "Invalid data line on line {}: '{}'. Expected 3 float values, found {}",
                            line_num,
                            line,
                            parts.len()
                        ));
                    }
                    let r: f32 = parts[0].parse().map_err(|e| {
                        anyhow!(
                            "Failed to parse R value on line {}: '{}'. Error: {}",
                            line_num,
                            line,
                            e
                        )
                    })?;
                    let g: f32 = parts[1].parse().map_err(|e| {
                        anyhow!(
                            "Failed to parse G value on line {}: '{}'. Error: {}",
                            line_num,
                            line,
                            e
                        )
                    })?;
                    let b: f32 = parts[2].parse().map_err(|e| {
                        anyhow!(
                            "Failed to parse B value on line {}: '{}'. Error: {}",
                            line_num,
                            line,
                            e
                        )
                    })?;
                    data.push(r);
                    data.push(g);
                    data.push(b);
                }
            }
        }
    }

    let lut_size = size.ok_or(anyhow!("LUT_3D_SIZE not found in .cube file"))?;
    let expected_len = (lut_size * lut_size * lut_size * 3) as usize;
    if data.len() != expected_len {
        return Err(anyhow!(
            "LUT data size mismatch. Expected {} float values (for size {}), but found {}. The file may be corrupt or incomplete.",
            expected_len,
            lut_size,
            data.len()
        ));
    }

    Ok(Lut {
        size: lut_size,
        data,
        shaper: None,
    })
}

/// Largest 3D grid a .cube may hold (the format's own limit) and largest 1D
/// table.
const MAX_3D_SIZE: u32 = 256;
const MAX_1D_SIZE: usize = 65536;
/// Grid a 1D table is baked into. A 1D curve sampled on the 3D
/// grid is interpolated linearly per channel between grid points
/// (tetrahedral interpolation of a per-channel curve is linear per
/// channel); a curve [`checked_bake`] finds that misses is read through the
/// shaper stage instead.
const BAKED_SIZE: u32 = 65;

/// Reads a .cube file (Adobe's Cube LUT Specification 1.0 and the
/// shaper form Resolve writes): a 3D table (`LUT_3D_SIZE`), a 1D table
/// (`LUT_1D_SIZE`) or a 1D shaper followed by a 3D table. The first table
/// spans `DOMAIN_MIN`/`DOMAIN_MAX` (per channel) or its
/// `LUT_1D_INPUT_RANGE`/`LUT_3D_INPUT_RANGE` (all channels); a shaper's 3D
/// table reads the shaper's output over its `LUT_3D_INPUT_RANGE`. A 3D
/// table over the unit domain is used as written; one after a shaper or
/// over another range gets a [`Shaper`] stage, which the pipeline samples
/// exactly before the table. A 1D table alone is baked into a
/// [`BAKED_SIZE`]³ table over the unit domain when that is within one 8-bit
/// code of the file's reading (provably, see [`checked_bake`]), otherwise
/// read through the shaper stage. Other keywords are ignored.
fn parse_cube_full(reader: impl BufRead) -> anyhow::Result<Lut> {
    let mut size_3d: Option<u32> = None;
    let mut size_1d: Option<usize> = None;
    let mut domain: Option<Domain> = None;
    let (mut domain_min, mut domain_max) = (None, None);
    let mut range_1d: Option<(f32, f32)> = None;
    let mut range_3d: Option<(f32, f32)> = None;
    let mut data: Vec<f32> = Vec::new();

    let floats = |parts: &[&str], count: usize, line_num: usize| -> anyhow::Result<Vec<f32>> {
        if parts.len() < count {
            return Err(anyhow!(
                "Line {line_num}: expected {count} values, found {}",
                parts.len()
            ));
        }
        parts[..count]
            .iter()
            .map(|p| {
                p.parse::<f32>()
                    .ok()
                    .filter(|v| v.is_finite())
                    .ok_or_else(|| anyhow!("Line {line_num}: '{p}' is not a number"))
            })
            .collect()
    };

    for (index, line) in reader.lines().enumerate() {
        let line_num = index + 1;
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let parts: Vec<&str> = trimmed.split_whitespace().collect();
        let keyword = parts[0].to_uppercase();
        let args = &parts[1..];
        match keyword.as_str() {
            "LUT_3D_SIZE" => {
                let size: u32 = args
                    .first()
                    .and_then(|v| v.parse().ok())
                    .ok_or_else(|| anyhow!("Line {line_num}: malformed LUT_3D_SIZE"))?;
                if !(2..=MAX_3D_SIZE).contains(&size) {
                    return Err(anyhow!("LUT_3D_SIZE {size} is outside 2–{MAX_3D_SIZE}"));
                }
                size_3d = Some(size);
            }
            "LUT_1D_SIZE" => {
                let size: usize = args
                    .first()
                    .and_then(|v| v.parse().ok())
                    .ok_or_else(|| anyhow!("Line {line_num}: malformed LUT_1D_SIZE"))?;
                if !(2..=MAX_1D_SIZE).contains(&size) {
                    return Err(anyhow!("LUT_1D_SIZE {size} is outside 2–{MAX_1D_SIZE}"));
                }
                size_1d = Some(size);
            }
            "DOMAIN_MIN" => {
                let v = floats(args, 3, line_num)?;
                domain_min = Some([v[0], v[1], v[2]]);
            }
            "DOMAIN_MAX" => {
                let v = floats(args, 3, line_num)?;
                domain_max = Some([v[0], v[1], v[2]]);
            }
            "LUT_1D_INPUT_RANGE" => {
                let v = floats(args, 2, line_num)?;
                range_1d = Some((v[0], v[1]));
            }
            "LUT_3D_INPUT_RANGE" => {
                let v = floats(args, 2, line_num)?;
                range_3d = Some((v[0], v[1]));
            }
            _ if keyword.starts_with(|c: char| c.is_ascii_alphabetic()) => {
                // TITLE and keywords of other writers; a data line starts
                // with a digit, a sign or a point.
                continue;
            }
            _ => {
                if size_3d.is_some() || size_1d.is_some() {
                    data.extend(floats(&parts, 3, line_num)?);
                }
            }
        }
    }
    if domain_min.is_some() || domain_max.is_some() {
        domain = Some((
            domain_min.unwrap_or([0.0; 3]),
            domain_max.unwrap_or([1.0; 3]),
        ));
    }

    let entries_1d = size_1d.unwrap_or(0);
    let entries_3d = size_3d.map_or(0, |n| (n * n * n) as usize);
    if size_3d.is_none() && size_1d.is_none() {
        return Err(anyhow!(
            "Neither LUT_3D_SIZE nor LUT_1D_SIZE found in .cube file"
        ));
    }
    let expected = (entries_1d + entries_3d) * 3;
    if data.len() != expected {
        return Err(anyhow!(
            "LUT data size mismatch. Expected {} float values, but found {}. The file may be corrupt or incomplete.",
            expected,
            data.len()
        ));
    }
    let unit = ([0.0; 3], [1.0; 3]);
    let from_range = |(lo, hi): (f32, f32)| ([lo; 3], [hi; 3]);
    // The file's domain applies to its first table; a shaper's 3D table
    // reads the shaper's output over its own input range.
    let domain_1d = range_1d.map(from_range).or(domain).unwrap_or(unit);
    let domain_3d = range_3d
        .map(from_range)
        .or(if size_1d.is_some() { None } else { domain })
        .unwrap_or(unit);
    for (axis, (lo, hi)) in [domain_1d, domain_3d]
        .iter()
        .flat_map(|(lo, hi)| lo.iter().zip(hi.iter()))
        .enumerate()
    {
        if hi <= lo {
            return Err(anyhow!(
                "Domain maximum {hi} is not above its minimum {lo} (channel {})",
                axis % 3
            ));
        }
    }

    let shaper = size_1d.map(|n| Shaper {
        size: n,
        data: data[..n * 3].to_vec(),
        domain: domain_1d,
        table_domain: domain_3d,
    });
    let table = |n: u32, shaper| Lut {
        size: n,
        data: data[entries_1d * 3..].to_vec(),
        shaper,
    };
    match (shaper, size_3d) {
        // As written (the parser before 1D tables read these).
        (None, Some(n)) if domain_3d == unit => Ok(table(n, None)),
        // A 3D table over another range: its range as a straight shaper.
        (None, Some(n)) => Ok(table(
            n,
            Some(Shaper {
                size: 2,
                data: domain_3d.0.into_iter().chain(domain_3d.1).collect(),
                domain: domain_3d,
                table_domain: domain_3d,
            }),
        )),
        // Baked into one grid, a 3D table read over another grid can
        // stray anywhere inside its cells, beyond what a check can bound;
        // the shaper stage reads the file exactly.
        (Some(shaper), Some(n)) => Ok(table(n, Some(shaper))),
        (Some(shaper), None) => {
            checked_bake(BAKED_SIZE, &breakpoints(&shaper), |rgb| shaper.apply(rgb))
                .or_else(|_| Ok(shaper.alone()))
        }
        (None, None) => Err(anyhow!("No table in .cube file")),
    }
}

/// A per-channel input range: minima and maxima.
pub type Domain = ([f32; 3], [f32; 3]);

/// A per-channel curve of `size` entries over `domain`, linearly
/// interpolated (inputs outside the domain take its end values), whose
/// output the 3D table reads over `table_domain`. Mirrored by the shader's
/// `lut_table_input`.
#[derive(Debug, Clone, PartialEq)]
pub struct Shaper {
    pub size: usize,
    /// `size` RGB entries.
    pub data: Vec<f32>,
    pub domain: Domain,
    pub table_domain: Domain,
}

impl Shaper {
    /// Where the 3D table is read for `rgb`: the curve's output mapped from
    /// `table_domain` to the unit cube, clamped.
    pub fn table_input(&self, rgb: [f32; 3]) -> [f32; 3] {
        normalize(self.apply(rgb), self.table_domain)
    }

    /// The curve on its own: a straight 2³ table spanning the curve's
    /// output range, which tetrahedral interpolation reads back exactly.
    fn alone(self) -> Lut {
        let mut lo = [f32::INFINITY; 3];
        let mut hi = [f32::NEG_INFINITY; 3];
        for entry in self.data.as_chunks::<3>().0 {
            for c in 0..3 {
                lo[c] = lo[c].min(entry[c]);
                hi[c] = hi[c].max(entry[c]);
            }
        }
        // A flat channel still needs a range to divide by.
        for c in 0..3 {
            if hi[c] <= lo[c] {
                hi[c] = lo[c] + 1.0;
            }
        }
        let mut lut = bake(2, |rgb| {
            std::array::from_fn(|c| lo[c] + (hi[c] - lo[c]) * rgb[c])
        });
        lut.shaper = Some(Shaper {
            table_domain: (lo, hi),
            ..self
        });
        lut
    }

    fn apply(&self, rgb: [f32; 3]) -> [f32; 3] {
        let x = normalize(rgb, self.domain);
        std::array::from_fn(|c| {
            let position = x[c] * (self.size - 1) as f32;
            let i = (position.floor() as usize).min(self.size - 2);
            let f = position - i as f32;
            let (a, b) = (self.data[i * 3 + c], self.data[(i + 1) * 3 + c]);
            a + (b - a) * f
        })
    }
}

/// `rgb` mapped from `domain` to the unit cube, clamped.
fn normalize(rgb: [f32; 3], (lo, hi): Domain) -> [f32; 3] {
    std::array::from_fn(|c| ((rgb[c] - lo[c]) / (hi[c] - lo[c])).clamp(0.0, 1.0))
}

/// Largest error a baked table may have against the file's own reading, in
/// output values: one 8-bit code.
const MAX_BAKE_ERROR: f32 = 1.0 / 255.0;

/// The inputs (unit cube, per channel) where a 1D table bends: its
/// entries.
fn breakpoints(shaper: &Shaper) -> [Vec<f32>; 3] {
    std::array::from_fn(|c| {
        let (lo, hi) = (shaper.domain.0[c], shaper.domain.1[c]);
        (0..shaper.size)
            .map(|i| lo + (hi - lo) * i as f32 / (shaper.size - 1) as f32)
            .filter(|x| (0.0..=1.0).contains(x))
            .collect()
    })
}

/// [`bake`], refused when the table strays from `transform` by more than
/// [`MAX_BAKE_ERROR`] (a curve too steep for the grid, such as a square
/// root near black). Measured at every entry of the 1D table
/// (`breakpoints`; on the grey axis there and with the other channels
/// scattered) and at 4096 grey and 4096 scattered colours. That is its
/// largest error: each output channel follows its own input channel only,
/// both readings are linear between their breakpoints, and the grid's own
/// points carry no error.
fn checked_bake(
    size: u32,
    breakpoints: &[Vec<f32>; 3],
    transform: impl Fn([f32; 3]) -> [f32; 3],
) -> anyhow::Result<Lut> {
    let lut = bake(size, &transform);
    let mut seed = 0x9e37_79b9u32;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        (seed >> 8) as f32 / (1u32 << 24) as f32
    };
    let mut probes: Vec<[f32; 3]> = (0..4096).map(|i| [i as f32 / 4095.0; 3]).collect();
    probes.extend((0..4096).map(|_| [next(), next(), next()]));
    for (c, points) in breakpoints.iter().enumerate() {
        for &x in points {
            probes.push([x; 3]);
            let mut scattered = [next(), next(), next()];
            scattered[c] = x;
            probes.push(scattered);
        }
    }
    let error = probes
        .into_iter()
        .map(|rgb| {
            let (got, want) = (lut.sample(rgb), transform(rgb));
            (0..3)
                .map(|c| (got[c] - want[c]).abs())
                .fold(0.0f32, f32::max)
        })
        .fold(0.0f32, f32::max);
    if error > MAX_BAKE_ERROR {
        return Err(anyhow!(
            "The curve is too steep for a {size}-point table (error {error:.4}, more than 1/255)"
        ));
    }
    Ok(lut)
}

/// `transform` sampled on a `size`³ grid over the unit cube (red fastest,
/// as a .cube lists it).
fn bake(size: u32, transform: impl Fn([f32; 3]) -> [f32; 3]) -> Lut {
    let step = 1.0 / (size - 1) as f32;
    let mut data = Vec::with_capacity((size * size * size * 3) as usize);
    for b in 0..size {
        for g in 0..size {
            for r in 0..size {
                data.extend(transform([
                    r as f32 * step,
                    g as f32 * step,
                    b as f32 * step,
                ]));
            }
        }
    }
    Lut {
        size,
        data,
        shaper: None,
    }
}

impl Lut {
    /// The table at `rgb` (unit cube, clamped; after the shaper when there
    /// is one) by tetrahedral interpolation, as the shader reads it
    /// (`sample_lut_tetrahedral` of `lut_table_input`).
    pub fn sample(&self, rgb: [f32; 3]) -> [f32; 3] {
        let rgb = match &self.shaper {
            Some(shaper) => shaper.table_input(rgb),
            None => rgb,
        };
        let n = self.size as usize;
        let scaled = rgb.map(|v| v.clamp(0.0, 1.0) * (n - 1) as f32);
        let base = scaled.map(|v| (v.floor() as usize).min(n - 1));
        let f: [f32; 3] = std::array::from_fn(|c| scaled[c] - base[c] as f32);
        let at = |dr: usize, dg: usize, db: usize| -> [f32; 3] {
            let (r, g, b) = (
                (base[0] + dr).min(n - 1),
                (base[1] + dg).min(n - 1),
                (base[2] + db).min(n - 1),
            );
            let i = ((b * n + g) * n + r) * 3;
            [self.data[i], self.data[i + 1], self.data[i + 2]]
        };
        // The tetrahedron holding `f`: its corners from (0,0,0) to (1,1,1)
        // in the order the fractions fall.
        let mut order = [0usize, 1, 2];
        order.sort_by(|&a, &b| f[b].total_cmp(&f[a]));
        let mut corner = [0usize; 3];
        let mut result = at(0, 0, 0).map(|v| v * (1.0 - f[order[0]]));
        for (k, &axis) in order.iter().enumerate() {
            corner[axis] = 1;
            let next = at(corner[0], corner[1], corner[2]);
            let weight = f[axis] - order.get(k + 1).map_or(0.0, |&a| f[a]);
            for c in 0..3 {
                result[c] += next[c] * weight;
            }
        }
        result
    }
}

/// The LUT at `amount` (0–2; the recipe's `lutIntensity` / 100) from the
/// colour without it (`base`) and with it at 100 % (`lut`), both display
/// values (sRGB-encoded, the LUT's own output space). Up to 1 a mix. Above
/// 1 the LUT's change is extrapolated, `lut + (amount − 1)·(lut − base)`,
/// with the extra part scaled down as a whole (its direction, the hue of
/// the change, kept) so that it never leaves the [0, 1] cube: by the
/// ratio r of the room left to the cube's nearest face over the extra
/// step, softened to r / (1 + r⁴)^¼ (≈ 1 for r ≥ 3, never above r). A
/// channel already outside the cube gets no extra step outward. Mirrored
/// by the shader's `lut_amount`.
pub fn apply_amount(base: [f32; 3], lut: [f32; 3], amount: f32) -> [f32; 3] {
    if amount <= 1.0 {
        return std::array::from_fn(|c| base[c] * (1.0 - amount) + lut[c] * amount);
    }
    let extra: [f32; 3] = std::array::from_fn(|c| (lut[c] - base[c]) * (amount - 1.0));
    let ratio = (0..3)
        .map(|c| {
            let room = if extra[c] > 0.0 { 1.0 - lut[c] } else { lut[c] }.max(0.0);
            let need = extra[c].abs();
            if need > 1e-6 {
                (room / need).min(64.0)
            } else {
                64.0
            }
        })
        .fold(64.0f32, f32::min);
    let scale = ratio / (1.0 + ratio.powi(4)).sqrt().sqrt();
    std::array::from_fn(|c| lut[c] + extra[c] * scale)
}

fn parse_3dl(reader: impl BufRead) -> anyhow::Result<Lut> {
    let mut data: Vec<f32> = Vec::new();

    for line in reader.lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let parts: Vec<&str> = trimmed.split_whitespace().collect();
        if parts.len() == 3 {
            let r: f32 = parts[0].parse()?;
            let g: f32 = parts[1].parse()?;
            let b: f32 = parts[2].parse()?;
            data.push(r);
            data.push(g);
            data.push(b);
        }
    }

    let total_values = data.len();
    if total_values == 0 {
        return Err(anyhow!("No data found in 3DL file"));
    }
    let num_entries = total_values / 3;
    let size = (num_entries as f64).cbrt().round() as u32;

    if size * size * size != num_entries as u32 {
        return Err(anyhow!(
            "Invalid 3DL LUT data size: the number of entries ({}) is not a perfect cube.",
            num_entries
        ));
    }

    Ok(Lut {
        size,
        data,
        shaper: None,
    })
}

fn parse_hald(image: DynamicImage) -> anyhow::Result<Lut> {
    let (width, height) = image.dimensions();
    if width != height {
        return Err(anyhow!(
            "HALD image must be square, but dimensions are {}x{}",
            width,
            height
        ));
    }

    let total_pixels = width * height;
    let size = (total_pixels as f64).cbrt().round() as u32;

    if size * size * size != total_pixels {
        return Err(anyhow!(
            "Invalid HALD image dimensions: total pixels ({}) is not a perfect cube.",
            total_pixels
        ));
    }

    let mut data = Vec::with_capacity((total_pixels * 3) as usize);
    let rgb_image = image.to_rgb8();

    for pixel in rgb_image.pixels() {
        data.push(pixel[0] as f32 / 255.0);
        data.push(pixel[1] as f32 / 255.0);
        data.push(pixel[2] as f32 / 255.0);
    }

    Ok(Lut {
        size,
        data,
        shaper: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cube(text: &str) -> anyhow::Result<Lut> {
        parse_cube(text.as_bytes().to_vec())
    }

    fn import(text: &str) -> anyhow::Result<Lut> {
        parse_lut_bytes_for_import("cube", text.as_bytes().to_vec())
    }

    /// A 3D .cube of `transform` on a `size`³ grid over `lo..hi` on every
    /// channel, with `header` lines before the data.
    fn table_3d(
        size: u32,
        header: &str,
        lo: f32,
        hi: f32,
        transform: impl Fn([f32; 3]) -> [f32; 3],
    ) -> String {
        let mut text = format!("TITLE \"t\"\n{header}LUT_3D_SIZE {size}\n");
        for b in 0..size {
            for g in 0..size {
                for r in 0..size {
                    let at = |i: u32| lo + (hi - lo) * i as f32 / (size - 1) as f32;
                    let [x, y, z] = transform([at(r), at(g), at(b)]);
                    text.push_str(&format!("{x:.7} {y:.7} {z:.7}\n"));
                }
            }
        }
        text
    }

    fn close(a: [f32; 3], b: [f32; 3], tolerance: f32) -> bool {
        (0..3).all(|c| (a[c] - b[c]).abs() <= tolerance)
    }

    #[test]
    fn a_plain_3d_table_is_used_as_written() {
        let look = |[r, g, b]: [f32; 3]| [r * 0.9 + 0.05, g * g, 1.0 - b];
        let plain = cube(&table_3d(9, "", 0.0, 1.0, look)).unwrap();
        let unit = cube(&table_3d(
            9,
            "DOMAIN_MIN 0 0 0\nDOMAIN_MAX 1.0 1.0 1.0\nLUT_3D_INPUT_RANGE 0 1\n",
            0.0,
            1.0,
            look,
        ))
        .unwrap();
        assert_eq!(plain.size, 9);
        assert_eq!(plain.data, unit.data);
        assert_eq!(plain.data[3..6], [0.9 * 0.125 + 0.05, 0.0, 1.0]);
    }

    #[test]
    fn a_1d_table_reads_as_its_per_channel_curve() {
        // Gamma 2 on red, inverted green, blue halved; 17 entries.
        let mut text = "LUT_1D_SIZE 17\nDOMAIN_MIN 0 0 0\nDOMAIN_MAX 1 1 1\n".to_string();
        for i in 0..17 {
            let x = i as f32 / 16.0;
            text.push_str(&format!("{} {} {}\n", x * x, 1.0 - x, x / 2.0));
        }
        let lut = cube(&text).unwrap();
        assert_eq!(lut.size, BAKED_SIZE);
        let curve = |x: f32| {
            // The table's own linear interpolation.
            let p = x * 16.0;
            let i = (p.floor() as usize).min(15);
            let (a, b) = ((i as f32 / 16.0).powi(2), ((i + 1) as f32 / 16.0).powi(2));
            a + (b - a) * (p - i as f32)
        };
        for &x in &[0.0, 0.1, 0.33, 0.5, 0.77, 0.98, 1.0] {
            let got = lut.sample([x, x * 0.5, 1.0 - x]);
            let want = [curve(x), 1.0 - x * 0.5, (1.0 - x) / 2.0];
            assert!(close(got, want, 2e-3), "{x}: {got:?} vs {want:?}");
        }
    }

    #[test]
    fn a_1d_domain_maps_the_input_range_onto_the_table() {
        // Identity table over 0..2: input 1.0 sits in the middle (0.5 out
        // of 0..1 table values would be 1.0 / 2).
        let text = "LUT_1D_SIZE 3\nDOMAIN_MIN 0 0 0\nDOMAIN_MAX 2 2 2\n0 0 0\n0.5 0.5 0.5\n1 1 1\n";
        let lut = cube(text).unwrap();
        assert!(close(lut.sample([1.0, 0.5, 0.0]), [0.5, 0.25, 0.0], 1e-5));
    }

    #[test]
    fn a_3d_domain_is_read_as_before_and_refused_on_import() {
        // An identity table over -0.25..1.25. The earlier reader ignored
        // the domain; saved edits keep that reading, a new import refuses
        // the file rather than depend on it.
        let text = table_3d(
            17,
            "DOMAIN_MIN -0.25 -0.25 -0.25\nDOMAIN_MAX 1.25 1.25 1.25\n",
            -0.25,
            1.25,
            |rgb| rgb,
        );
        let lut = cube(&text).unwrap();
        assert_eq!(lut.size, 17);
        assert!(close(lut.sample([0.0; 3]), [-0.25; 3], 1e-6));
        assert!(import(&text).unwrap_err().to_string().contains("domain"));
        // Over the unit domain both readings agree: imported as written.
        let unit = table_3d(9, "DOMAIN_MIN 0 0 0\nDOMAIN_MAX 1 1 1\n", 0.0, 1.0, |rgb| {
            rgb
        });
        assert_eq!(import(&unit).unwrap().data, cube(&unit).unwrap().data);
    }

    /// The curve a .cube's 1D table describes at `x`: its entries over
    /// `lo..hi`, linear between them, end values outside.
    fn curve(entries: &[f32], lo: f32, hi: f32, x: f32) -> f32 {
        let p = ((x - lo) / (hi - lo)).clamp(0.0, 1.0) * (entries.len() - 1) as f32;
        let i = (p.floor() as usize).min(entries.len() - 2);
        entries[i] + (entries[i + 1] - entries[i]) * (p - i as f32)
    }

    /// Inputs over and beyond the unit cube: the grey axis, then scattered
    /// colours.
    fn probes() -> Vec<[f32; 3]> {
        let mut seed = 0x2545_f491u32;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            (seed >> 8) as f32 / (1u32 << 24) as f32 * 1.2 - 0.1
        };
        let mut probes: Vec<[f32; 3]> = (0..=512).map(|i| [i as f32 / 512.0; 3]).collect();
        probes.extend((0..4096).map(|_| [next(), next(), next()]));
        probes
    }

    #[test]
    fn a_curve_too_steep_for_the_table_is_read_through_the_shaper() {
        // Square root over 1025 entries: near black the 65-point grid
        // misses it by far more than one 8-bit code; the shaper stage reads
        // the file's own curve.
        let entries: Vec<f32> = (0..1025).map(|i| (i as f32 / 1024.0).sqrt()).collect();
        let mut text = "LUT_1D_SIZE 1025\n".to_string();
        for v in &entries {
            text.push_str(&format!("{v} {v} {v}\n"));
        }
        let lut = cube(&text).unwrap();
        assert_eq!(lut.size, 2);
        assert!(lut.shaper.is_some());
        for rgb in probes() {
            let want = rgb.map(|x| curve(&entries, 0.0, 1.0, x));
            assert!(close(lut.sample(rgb), want, 1e-6), "{rgb:?}");
        }
        assert_eq!(import(&text).unwrap(), lut);
        // A spike between the grid's points (65536 entries, one red entry
        // lit) is read as written, the flat channels too.
        let mut spike = "LUT_1D_SIZE 65536\n".to_string();
        for i in 0..65536 {
            spike.push_str(if i == 1000 { "1 0.5 0\n" } else { "0 0.5 0\n" });
        }
        let lut = cube(&spike).unwrap();
        let at = |i: f32| i / 65535.0;
        assert!(close(
            lut.sample([at(1000.0), 0.3, 0.9]),
            [1.0, 0.5, 0.0],
            1e-6
        ));
        assert!(close(
            lut.sample([at(1000.5), 0.0, 0.0]),
            [0.5, 0.5, 0.0],
            1e-4
        ));
        assert!(close(
            lut.sample([at(1002.0), 1.0, 1.0]),
            [0.0, 0.5, 0.0],
            1e-6
        ));
        // A gentle curve (gamma 1/1.2) is within the bound: baked as
        // before.
        let mut text = "LUT_1D_SIZE 1025\n".to_string();
        for i in 0..1025 {
            let v = (i as f32 / 1024.0).powf(1.0 / 1.2);
            text.push_str(&format!("{v} {v} {v}\n"));
        }
        let lut = cube(&text).unwrap();
        assert_eq!((lut.size, lut.shaper), (BAKED_SIZE, None));
    }

    /// Resolve's form: a 1D shaper of `entries` per channel over
    /// `range_1d`, then a 3D table of `look` over `range_3d`.
    fn shaper_cube(
        entries: &[[f32; 3]],
        range_1d: (f32, f32),
        size: u32,
        range_3d: (f32, f32),
        look: impl Fn([f32; 3]) -> [f32; 3],
    ) -> String {
        let mut text = format!(
            "TITLE \"s\"\nLUT_1D_SIZE {}\nLUT_1D_INPUT_RANGE {} {}\nLUT_3D_SIZE {size}\nLUT_3D_INPUT_RANGE {} {}\n",
            entries.len(),
            range_1d.0,
            range_1d.1,
            range_3d.0,
            range_3d.1
        );
        for [r, g, b] in entries {
            text.push_str(&format!("{r:.9} {g:.9} {b:.9}\n"));
        }
        let table = table_3d(size, "", range_3d.0, range_3d.1, look);
        text.push_str(
            table
                .split_once(&format!("LUT_3D_SIZE {size}\n"))
                .unwrap()
                .1,
        );
        text
    }

    #[test]
    fn a_3d_table_after_a_log_shaper_reads_the_shaper_output() {
        // A log-style shaper: scene values 0..16 to log2 over -8..4 stops,
        // mapped to 0.05..0.95 (beyond the 3D range at the ends), then an
        // affine look over 0..1, which tetrahedral interpolation reads
        // exactly: the result is the look of the file's own curve.
        let log = |x: f32| 0.05 + 0.9 * ((x.max(1.0 / 256.0)).log2() + 8.0) / 12.0;
        let entries: Vec<[f32; 3]> = (0..4096)
            .map(|i| {
                let x = 16.0 * i as f32 / 4095.0;
                [log(x), log(x * 0.9), log(x * 1.1)]
            })
            .collect();
        let look = |[r, g, b]: [f32; 3]| [0.8 * r + 0.1 * g, 0.9 * g + 0.05, 0.3 * r + 0.6 * b];
        let text = shaper_cube(&entries, (0.0, 16.0), 17, (0.0, 1.0), look);
        let lut = cube(&text).unwrap();
        assert_eq!(lut.size, 17);
        let channel = |c: usize| entries.iter().map(|e| e[c]).collect::<Vec<_>>();
        let channels = [channel(0), channel(1), channel(2)];
        for rgb in probes().into_iter().map(|p| p.map(|v| v * 16.0)) {
            let shaped: [f32; 3] =
                std::array::from_fn(|c| curve(&channels[c], 0.0, 16.0, rgb[c]).clamp(0.0, 1.0));
            assert!(close(lut.sample(rgb), look(shaped), 2e-6), "{rgb:?}");
        }
        // Imported as read: the earlier reader takes no such file.
        assert!(parse_cube_3d(BufReader::new(Cursor::new(text.as_bytes()))).is_err());
        assert_eq!(import(&text).unwrap(), lut);
    }

    #[test]
    fn a_shaper_table_is_read_inside_its_cells() {
        // The review's counterexample to a baked grid: a near-linear
        // shaper and a 9³ identity table with one lit entry. Between the
        // grid points the result is the table's own interpolation.
        let entries = [[0.0078125; 3], [1.0; 3]];
        let mut text = shaper_cube(&entries, (0.0, 1.0), 9, (0.0, 1.0), |rgb| rgb);
        // Light the entry at (4, 4, 4): line (4·81 + 4·9 + 4) of the table.
        let lit = 4 * 81 + 4 * 9 + 4;
        let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
        let first = lines.len() - 729;
        lines[first + lit] = "1 0 0".to_string();
        text = lines.join("\n");
        let lut = cube(&text).unwrap();
        let shaped = |x: f32| 0.0078125 + (1.0 - 0.0078125) * x;
        // The input whose shaped value lands on the lit point, and one an
        // eighth of a cell above it on every channel.
        let on = (0.5 - 0.0078125) / (1.0 - 0.0078125);
        assert!(close(lut.sample([on; 3]), [1.0, 0.0, 0.0], 1e-5));
        let x = on + 0.125 / 8.0 / (1.0 - 0.0078125);
        let t = (shaped(x) - 0.5) * 8.0;
        // Tetrahedral weight of the lit corner on the diagonal is 1 − t.
        let want_r = (1.0 - t) * 1.0 + t * 0.625;
        let want_gb = t * 0.625;
        assert!(close(lut.sample([x; 3]), [want_r, want_gb, want_gb], 1e-5));
    }

    #[test]
    fn a_3d_table_over_another_range_reads_its_range() {
        // An identity table over -0.25..1.25 (LUT_3D_INPUT_RANGE after the
        // size, which the earlier reader took for a data line and refused):
        // read over its range, so it stays an identity.
        let ranged = table_3d(9, "", -0.25, 1.25, |rgb| rgb).replacen(
            "LUT_3D_SIZE 9\n",
            "LUT_3D_SIZE 9\nLUT_3D_INPUT_RANGE -0.25 1.25\n",
            1,
        );
        assert!(parse_cube_3d(BufReader::new(Cursor::new(ranged.as_bytes()))).is_err());
        let lut = cube(&ranged).unwrap();
        for rgb in probes() {
            assert!(close(lut.sample(rgb), rgb, 2e-6), "{rgb:?}");
        }
        assert_eq!(import(&ranged).unwrap(), lut);
        // A shaper whose output the 3D table reads over 0..2: half of an
        // identity table's span, so values come out halved.
        let text = shaper_cube(&[[0.0; 3], [2.0; 3]], (0.0, 1.0), 5, (0.0, 2.0), |rgb| {
            rgb.map(|v| v / 2.0)
        });
        let lut = cube(&text).unwrap();
        assert!(close(lut.sample([0.3, 0.6, 1.0]), [0.3, 0.6, 1.0], 2e-6));
        assert!(close(lut.sample([0.3, -0.5, 1.5]), [0.3, 0.0, 1.0], 2e-6));
        // Before the size the earlier reader ignored the line: saved edits
        // keep that reading, an import refuses it.
        let before = table_3d(9, "LUT_3D_INPUT_RANGE -0.25 1.25\n", -0.25, 1.25, |rgb| rgb);
        assert_eq!(cube(&before).unwrap().shaper, None);
        assert!(import(&before).is_err());
    }

    #[test]
    fn unsupported_cubes_say_why() {
        let reason = |text: &str| cube(text).unwrap_err().to_string();
        assert!(reason("TITLE x\n0 0 0\n").contains("LUT_3D_SIZE nor LUT_1D_SIZE"));
        assert!(reason("LUT_3D_SIZE 2\n0 0 0\n").contains("mismatch"));
        assert!(reason("LUT_1D_SIZE 1\n0 0 0\n").contains("outside"));
        assert!(
            reason("LUT_1D_SIZE 2\nDOMAIN_MIN 1 0 0\nDOMAIN_MAX 1 1 1\n0 0 0\n1 1 1\n")
                .contains("not above")
        );
        assert!(reason("LUT_1D_SIZE 2\n0 0 0\n1 x 1\n").contains("not a number"));
        assert!(reason("LUT_1D_SIZE 2\n0 0 0\n1 inf 1\n").contains("not a number"));
    }

    #[test]
    fn only_parent_folder_components_count_as_traversal() {
        assert!(validate_lut_path("/luts/Pack..v2/Tone..v2.cube").is_ok());
        assert!(validate_lut_path("/luts/../photo.cube").is_err());
        assert!(validate_lut_path("../photo.cube").is_err());
        assert!(validate_lut_path("/luts/a/..").is_err());
    }

    #[test]
    fn up_to_100_percent_the_amount_mixes() {
        let (base, lut) = ([0.2, 0.5, 0.9], [0.3, 0.4, 0.95]);
        for amount in [0.0, 0.4, 1.0] {
            let got = apply_amount(base, lut, amount);
            for c in 0..3 {
                assert_eq!(got[c], base[c] * (1.0 - amount) + lut[c] * amount);
            }
        }
    }

    #[test]
    fn above_100_percent_the_change_is_extrapolated_inside_the_cube() {
        // Far from the cube's faces: the plain extrapolation.
        let (base, lut) = ([0.40, 0.50, 0.45], [0.44, 0.48, 0.45]);
        assert!(close(
            apply_amount(base, lut, 2.0),
            [0.48, 0.46, 0.45],
            1e-4
        ));
        assert!(close(
            apply_amount(base, lut, 1.5),
            [0.46, 0.47, 0.45],
            1e-4
        ));
        // Continuous at 100 %.
        assert!(close(apply_amount(base, lut, 1.0 + 1e-4), lut, 1e-5));
        // Near a face: never beyond it, the change's direction kept.
        let (base, lut) = ([0.70, 0.30, 0.20], [0.90, 0.25, 0.20]);
        let got = apply_amount(base, lut, 2.0);
        assert!(got[0] < 1.0 && got[0] > 0.95, "{got:?}");
        let ratio = (got[1] - lut[1]) / (got[0] - lut[0]);
        assert!((ratio - (-0.05 / 0.20)).abs() < 1e-4);
        // A channel already beyond the cube gets no step further out.
        let got = apply_amount([0.9, 0.5, 0.5], [1.1, 0.5, 0.5], 2.0);
        assert_eq!(got, [1.1, 0.5, 0.5]);
        // Over a grid of colours and amounts: inside the cube, between
        // the 100 % colour and the plain extrapolation.
        let steps = [0.0f32, 0.03, 0.2, 0.5, 0.8, 0.97, 1.0];
        for &a in &steps {
            for &b in &steps {
                for &c in &steps {
                    let base = [a, b, c];
                    let lut = [b, c * 0.8 + 0.1, (a + 0.3).min(1.0)];
                    for amount in [1.25, 1.5, 2.0] {
                        let got = apply_amount(base, lut, amount);
                        for k in 0..3 {
                            assert!((0.0..=1.0).contains(&got[k]), "{base:?} {amount}: {got:?}");
                            let far = lut[k] + (lut[k] - base[k]) * (amount - 1.0);
                            assert!((got[k] - lut[k]) * (far - got[k]) >= -1e-6);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn sampling_matches_the_table_at_its_points_and_between() {
        let lut = bake(5, |[r, g, b]| [r, g * 0.5, (r + b) / 2.0]);
        assert!(close(
            lut.sample([0.25, 0.75, 1.0]),
            [0.25, 0.375, 0.625],
            1e-6
        ));
        // Linear transforms are exact between the points too.
        assert!(close(
            lut.sample([0.1, 0.62, 0.33]),
            [0.1, 0.31, 0.215],
            1e-6
        ));
        assert!(close(lut.sample([1.4, -0.2, 0.5]), [1.0, 0.0, 0.75], 1e-6));
    }
}
