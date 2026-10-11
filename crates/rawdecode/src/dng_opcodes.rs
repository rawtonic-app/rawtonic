//! Lens corrections embedded in DNG files (OpcodeList3).
//!
//! DNG writers such as DJI drones store their lens shading and distortion
//! corrections as opcodes that a raw converter applies to the demosaiced
//! camera-space image ("stage 3", before the default crop). Camera Raw and
//! Lightroom always apply them; without them small-sensor DNGs keep up to
//! 2 EV of corner vignetting and a colour cast toward the corners.
//!
//! Supported: GainMap (9), FixVignetteRadial (3) and WarpRectilinear (1);
//! MapPolynomial (8) in OpcodeList2 of three-colour linear DNGs.
//! Other opcodes are skipped with a log line. rawler has already converted the
//! image to linear sRGB, which is linear in camera RGB (white balance and the
//! camera matrix), so a per-plane camera gain g is applied exactly as
//! `M · diag(g) · M⁻¹` with M the camera-to-sRGB matrix.
use rayon::prelude::*;

const WARP_RECTILINEAR: u32 = 1;
const FIX_VIGNETTE_RADIAL: u32 = 3;
const MAP_POLYNOMIAL: u32 = 8;
const GAIN_MAP: u32 = 9;

/// Camera-to-output matrix and its inverse.
type Conjugation = ([[f32; 3]; 3], [[f32; 3]; 3]);

#[derive(Debug, Clone, PartialEq)]
pub struct GainMap {
    pub top: u32,
    pub left: u32,
    pub bottom: u32,
    pub right: u32,
    pub plane: u32,
    pub planes: u32,
    pub row_pitch: u32,
    pub col_pitch: u32,
    pub points_v: usize,
    pub points_h: usize,
    pub spacing_v: f64,
    pub spacing_h: f64,
    pub origin_v: f64,
    pub origin_h: f64,
    pub map_planes: usize,
    pub gains: Vec<f32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WarpRectilinear {
    /// Per plane: kr0..kr3, kt0, kt1.
    pub coefficients: Vec<[f64; 6]>,
    pub centre: (f64, f64),
}

#[derive(Debug, Clone, PartialEq)]
pub struct FixVignetteRadial {
    pub k: [f64; 5],
    pub centre: (f64, f64),
}

/// A polynomial applied to one plane's normalised values (0 = black,
/// 1 = white) in an area: Lightroom's panoramas of integer raws store
/// sRGB-encoded samples and undo the encoding with one per colour.
#[derive(Debug, Clone, PartialEq)]
pub struct MapPolynomial {
    pub top: u32,
    pub left: u32,
    pub bottom: u32,
    pub right: u32,
    pub plane: u32,
    pub planes: u32,
    pub row_pitch: u32,
    pub col_pitch: u32,
    /// c0, c1, … (value = Σ cᵢ·vⁱ).
    pub coefficients: Vec<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Opcode {
    GainMap(GainMap),
    FixVignetteRadial(FixVignetteRadial),
    WarpRectilinear(WarpRectilinear),
    MapPolynomial(MapPolynomial),
}

/// Where the developed image sits inside the stage-3 image, in developed
/// pixels (the developed image may be cropped and, for fast demosaicing,
/// scaled down).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stage3Frame {
    pub width: f64,
    pub height: f64,
    pub left: f64,
    pub top: f64,
    /// Developed pixels per stage-3 pixel.
    pub scale: f64,
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
    little_endian: bool,
}

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Option<&[u8]> {
        let slice = self.bytes.get(self.pos..self.pos.checked_add(n)?)?;
        self.pos += n;
        Some(slice)
    }
    fn u32(&mut self) -> Option<u32> {
        let b: [u8; 4] = self.take(4)?.try_into().ok()?;
        Some(if self.little_endian {
            u32::from_le_bytes(b)
        } else {
            u32::from_be_bytes(b)
        })
    }
    fn f32(&mut self) -> Option<f32> {
        Some(f32::from_bits(self.u32()?))
    }
    fn f64(&mut self) -> Option<f64> {
        let b: [u8; 8] = self.take(8)?.try_into().ok()?;
        Some(if self.little_endian {
            f64::from_le_bytes(b)
        } else {
            f64::from_be_bytes(b)
        })
    }
}

/// Parses an opcode list (always big-endian). Unknown or malformed opcodes
/// are skipped; a truncated list returns the opcodes read so far.
pub fn parse_opcode_list(bytes: &[u8]) -> Vec<Opcode> {
    let mut reader = Reader {
        bytes,
        pos: 0,
        little_endian: false,
    };
    let Some(count) = reader.u32() else {
        return Vec::new();
    };
    let mut opcodes = Vec::new();
    for _ in 0..count.min(256) {
        let (Some(id), Some(_version), Some(_flags), Some(size)) =
            (reader.u32(), reader.u32(), reader.u32(), reader.u32())
        else {
            break;
        };
        let Some(body) = reader.take(size as usize) else {
            break;
        };
        let parsed = match id {
            GAIN_MAP => parse_gain_map(body).map(Opcode::GainMap),
            WARP_RECTILINEAR => parse_warp(body).map(Opcode::WarpRectilinear),
            FIX_VIGNETTE_RADIAL => parse_vignette(body).map(Opcode::FixVignetteRadial),
            MAP_POLYNOMIAL => parse_map_polynomial(body).map(Opcode::MapPolynomial),
            other => {
                log::info!("DNG opcode {other} is not supported; skipped");
                None
            }
        };
        if let Some(opcode) = parsed {
            opcodes.push(opcode);
        }
    }
    opcodes
}

fn parse_map_polynomial(body: &[u8]) -> Option<MapPolynomial> {
    let mut r = Reader {
        bytes: body,
        pos: 0,
        little_endian: false,
    };
    let (top, left, bottom, right) = (r.u32()?, r.u32()?, r.u32()?, r.u32()?);
    let (plane, planes, row_pitch, col_pitch) = (r.u32()?, r.u32()?, r.u32()?, r.u32()?);
    let degree = r.u32()?;
    if degree > 8 || planes == 0 || row_pitch == 0 || col_pitch == 0 {
        return None;
    }
    let coefficients = (0..=degree)
        .map(|_| r.f64())
        .collect::<Option<Vec<f64>>>()?;
    Some(MapPolynomial {
        top,
        left,
        bottom,
        right,
        plane,
        planes,
        row_pitch,
        col_pitch,
        coefficients,
    })
}

/// Applies OpcodeList2 MapPolynomials to interleaved samples (`cpp` per
/// pixel, `width` pixels per row) whose black is `black` and white `white`
/// per plane; the polynomial acts on the normalised value. Returns the
/// number applied.
pub fn apply_map_polynomials(
    opcodes: &[Opcode],
    data: &mut [f32],
    width: usize,
    cpp: usize,
    black: &[f32],
    white: &[f32],
) -> usize {
    if width == 0 || cpp == 0 {
        return 0;
    }
    let height = data.len() / (width * cpp);
    let mut applied = 0;
    for opcode in opcodes {
        let Opcode::MapPolynomial(map) = opcode else {
            continue;
        };
        let (top, left) = (map.top as usize, map.left as usize);
        let (bottom, right) = (
            (map.bottom as usize).min(height),
            (map.right as usize).min(width),
        );
        let planes = map.plane as usize..(map.plane as usize + map.planes as usize).min(cpp);
        if top >= bottom || left >= right || planes.is_empty() {
            continue;
        }
        let c: Vec<f32> = map.coefficients.iter().map(|&v| v as f32).collect();
        let (rp, cp) = (map.row_pitch as usize, map.col_pitch as usize);
        data.par_chunks_exact_mut(width * cpp)
            .enumerate()
            .filter(|(y, _)| (top..bottom).contains(y) && (y - top) % rp == 0)
            .for_each(|(_, row)| {
                for x in (left..right).step_by(cp) {
                    for p in planes.clone() {
                        let (b, w) = (
                            black.get(p).or(black.first()).copied().unwrap_or(0.0),
                            white.get(p).or(white.first()).copied().unwrap_or(1.0),
                        );
                        let range = (w - b).max(f32::MIN_POSITIVE);
                        let s = &mut row[x * cpp + p];
                        let v = (*s - b) / range;
                        let mapped = c.iter().rev().fold(0.0f32, |acc, k| acc * v + k);
                        *s = mapped * range + b;
                    }
                }
            });
        applied += 1;
    }
    applied
}

fn parse_gain_map(body: &[u8]) -> Option<GainMap> {
    let mut r = Reader {
        bytes: body,
        pos: 0,
        little_endian: false,
    };
    let mut map = GainMap {
        top: r.u32()?,
        left: r.u32()?,
        bottom: r.u32()?,
        right: r.u32()?,
        plane: r.u32()?,
        planes: r.u32()?,
        row_pitch: r.u32()?,
        col_pitch: r.u32()?,
        points_v: r.u32()? as usize,
        points_h: r.u32()? as usize,
        spacing_v: r.f64()?,
        spacing_h: r.f64()?,
        origin_v: r.f64()?,
        origin_h: r.f64()?,
        map_planes: r.u32()? as usize,
        gains: Vec::new(),
    };
    let count = map
        .points_v
        .checked_mul(map.points_h)?
        .checked_mul(map.map_planes)?;
    if count == 0 || count > 1 << 20 || map.planes == 0 {
        return None;
    }
    map.gains = (0..count).map(|_| r.f32()).collect::<Option<_>>()?;
    Some(map)
}

fn parse_warp(body: &[u8]) -> Option<WarpRectilinear> {
    let mut r = Reader {
        bytes: body,
        pos: 0,
        little_endian: false,
    };
    let planes = r.u32()?;
    if planes == 0 || planes > 4 {
        return None;
    }
    let mut coefficients = Vec::new();
    for _ in 0..planes {
        let mut k = [0.0; 6];
        for value in k.iter_mut() {
            *value = r.f64()?;
        }
        coefficients.push(k);
    }
    let centre = (r.f64()?, r.f64()?);
    Some(WarpRectilinear {
        coefficients,
        centre,
    })
}

fn parse_vignette(body: &[u8]) -> Option<FixVignetteRadial> {
    let mut r = Reader {
        bytes: body,
        pos: 0,
        little_endian: false,
    };
    let mut k = [0.0; 5];
    for value in k.iter_mut() {
        *value = r.f64()?;
    }
    let centre = (r.f64()?, r.f64()?);
    Some(FixVignetteRadial { k, centre })
}

impl WarpRectilinear {
    fn is_identity(&self) -> bool {
        self.coefficients
            .iter()
            .all(|k| (k[0] - 1.0).abs() < 1e-9 && k[1..].iter().all(|value| value.abs() < 1e-9))
    }
}

/// Map sample pair and weight for a stage-3 position relative to the image
/// size (pixel centres), clamped at the map edges.
fn map_position(position: f64, origin: f64, spacing: f64, points: usize) -> (usize, usize, f32) {
    let f = ((position - origin) / spacing.max(1e-12)).clamp(0.0, (points - 1) as f64);
    let i0 = f.floor() as usize;
    (i0, (i0 + 1).min(points - 1), (f - i0 as f64) as f32)
}

fn invert3(m: &[[f32; 3]; 3]) -> Option<[[f32; 3]; 3]> {
    let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
    if det.abs() < 1e-12 {
        return None;
    }
    let d = 1.0 / det;
    Some([
        [
            (m[1][1] * m[2][2] - m[1][2] * m[2][1]) * d,
            (m[0][2] * m[2][1] - m[0][1] * m[2][2]) * d,
            (m[0][1] * m[1][2] - m[0][2] * m[1][1]) * d,
        ],
        [
            (m[1][2] * m[2][0] - m[1][0] * m[2][2]) * d,
            (m[0][0] * m[2][2] - m[0][2] * m[2][0]) * d,
            (m[0][2] * m[1][0] - m[0][0] * m[1][2]) * d,
        ],
        [
            (m[1][0] * m[2][1] - m[1][1] * m[2][0]) * d,
            (m[0][1] * m[2][0] - m[0][0] * m[2][1]) * d,
            (m[0][0] * m[1][1] - m[0][1] * m[1][0]) * d,
        ],
    ])
}

fn mul3(m: &[[f32; 3]; 3], p: [f32; 3]) -> [f32; 3] {
    [
        m[0][0] * p[0] + m[0][1] * p[1] + m[0][2] * p[2],
        m[1][0] * p[0] + m[1][1] * p[1] + m[1][2] * p[2],
        m[2][0] * p[0] + m[2][1] * p[1] + m[2][2] * p[2],
    ]
}

/// Applies the opcodes in order to an RGB image (`pixels` row-major,
/// `width` × `height`). `camera_to_output` is the camera-to-output matrix
/// (None when the pixels are still camera RGB, e.g. monochrome or
/// uncalibrated linear DNGs). Returns how many opcodes were applied.
pub fn apply_opcodes(
    opcodes: &[Opcode],
    pixels: &mut [[f32; 3]],
    width: usize,
    height: usize,
    frame: Stage3Frame,
    camera_to_output: Option<[[f32; 3]; 3]>,
) -> usize {
    if pixels.len() != width * height || width == 0 || height == 0 {
        return 0;
    }
    let conjugation = camera_to_output.and_then(|m| invert3(&m).map(|inverse| (m, inverse)));
    let mut applied = 0;
    for opcode in opcodes {
        let done = match opcode {
            Opcode::GainMap(map) => apply_gain_map(map, pixels, width, frame, conjugation),
            Opcode::FixVignetteRadial(vignette) => {
                apply_vignette(vignette, pixels, width, frame);
                true
            }
            Opcode::WarpRectilinear(warp) => apply_warp(warp, pixels, width, height, frame),
            // Stage 2 only (the image here is already in output colours).
            Opcode::MapPolynomial(_) => false,
        };
        applied += usize::from(done);
    }
    applied
}

fn apply_gain_map(
    map: &GainMap,
    pixels: &mut [[f32; 3]],
    width: usize,
    frame: Stage3Frame,
    conjugation: Option<Conjugation>,
) -> bool {
    // Pitches above one address CFA sub-grids; they belong in OpcodeList2.
    if map.row_pitch != 1 || map.col_pitch != 1 || map.plane >= 3 {
        log::info!("DNG GainMap with pitch/plane layout not supported; skipped");
        return false;
    }
    let first = map.plane as usize;
    let last = (first + map.planes as usize).min(3);
    let (top, bottom) = (
        map.top as f64 * frame.scale,
        map.bottom as f64 * frame.scale,
    );
    let (left, right) = (
        map.left as f64 * frame.scale,
        map.right as f64 * frame.scale,
    );
    // Horizontal map positions are shared by every row.
    let columns: Vec<Option<(usize, usize, f32)>> = (0..width)
        .map(|x| {
            let sx = x as f64 + frame.left + 0.5;
            (sx >= left && sx < right)
                .then(|| map_position(sx / frame.width, map.origin_h, map.spacing_h, map.points_h))
        })
        .collect();
    pixels
        .par_chunks_exact_mut(width)
        .enumerate()
        .for_each(|(y, row)| {
            let sy = y as f64 + frame.top + 0.5;
            if sy < top || sy >= bottom {
                return;
            }
            // Interpolate the map vertically once per row.
            let (v0, v1, tv) =
                map_position(sy / frame.height, map.origin_v, map.spacing_v, map.points_v);
            let row_gains: Vec<[f32; 3]> = (0..map.points_h)
                .map(|col| {
                    let mut gain = [1.0f32; 3];
                    for (plane, value) in gain.iter_mut().enumerate().take(last).skip(first) {
                        let map_plane = (plane - first).min(map.map_planes - 1);
                        let at = |r: usize| {
                            map.gains[(r * map.points_h + col) * map.map_planes + map_plane]
                        };
                        *value = at(v0) + (at(v1) - at(v0)) * tv;
                    }
                    gain
                })
                .collect();
            for (pixel, column) in row.iter_mut().zip(&columns) {
                let Some((h0, h1, th)) = *column else {
                    continue;
                };
                let (g0, g1) = (row_gains[h0], row_gains[h1]);
                let gain = [
                    g0[0] + (g1[0] - g0[0]) * th,
                    g0[1] + (g1[1] - g0[1]) * th,
                    g0[2] + (g1[2] - g0[2]) * th,
                ];
                match conjugation {
                    Some((m, inverse)) => {
                        let mut camera = mul3(&inverse, *pixel);
                        for c in 0..3 {
                            camera[c] *= gain[c];
                        }
                        let out = mul3(&m, camera);
                        *pixel = [out[0].max(0.0), out[1].max(0.0), out[2].max(0.0)];
                    }
                    None => {
                        for c in 0..3 {
                            pixel[c] *= gain[c];
                        }
                    }
                }
            }
        });
    true
}

/// Applies OpcodeList2 gain maps to normalised CFA data (stage 2: black
/// subtracted, before demosaicing). `active` is the active area (left, top,
/// width, height) in raw pixels; map positions are relative to it, as in the
/// DNG SDK. Phones store their per-CFA-colour lens shading here, one map per
/// Bayer position (row and column pitch 2). Returns the number applied.
pub fn apply_cfa_gain_maps(
    opcodes: &[Opcode],
    data: &mut [f32],
    raw_width: usize,
    active: (usize, usize, usize, usize),
) -> usize {
    let (active_left, active_top, active_width, active_height) = active;
    if raw_width == 0 || active_width == 0 || active_height == 0 {
        return 0;
    }
    let maps: Vec<&GainMap> = opcodes
        .iter()
        .filter_map(|opcode| match opcode {
            Opcode::GainMap(map) if map.plane == 0 && map.row_pitch > 0 && map.col_pitch > 0 => {
                Some(map)
            }
            _ => None,
        })
        .collect();
    if maps.is_empty() {
        return 0;
    }
    let (width, height) = (active_width as f64, active_height as f64);
    data.par_chunks_exact_mut(raw_width)
        .enumerate()
        .skip(active_top)
        .take(active_height)
        .for_each(|(raw_y, row)| {
            let y = raw_y - active_top;
            let v = (y as f64 + 0.5) / height;
            for map in &maps {
                let (top, bottom) = (map.top as usize, map.bottom as usize);
                if y < top || y >= bottom || (y - top) % map.row_pitch as usize != 0 {
                    continue;
                }
                let (v0, v1, tv) = map_position(v, map.origin_v, map.spacing_v, map.points_v);
                let left = map.left as usize;
                let right = (map.right as usize).min(active_width);
                let mut x = left;
                while x < right {
                    let h = (x as f64 + 0.5) / width;
                    let (h0, h1, th) = map_position(h, map.origin_h, map.spacing_h, map.points_h);
                    let at =
                        |r: usize, c: usize| map.gains[(r * map.points_h + c) * map.map_planes];
                    let top_gain = at(v0, h0) + (at(v0, h1) - at(v0, h0)) * th;
                    let bottom_gain = at(v1, h0) + (at(v1, h1) - at(v1, h0)) * th;
                    if let Some(value) = row.get_mut(active_left + x) {
                        *value *= top_gain + (bottom_gain - top_gain) * tv;
                    }
                    x += map.col_pitch as usize;
                }
            }
        });
    maps.len()
}

/// DNG 1.6 ProfileGainTableMap: a local tone map (Apple ProRAW). For every
/// pixel a weighted value of (R, G, B, min, max) indexes a 1-D gain table,
/// interpolated between spatial map points; the pixel is multiplied by the
/// gain. Stored in the file's byte order (ProRAW big-endian, Pixel
/// little-endian): the order whose header matches the data length is used.
#[derive(Debug, Clone, PartialEq)]
pub struct GainTableMap {
    pub points_v: usize,
    pub points_h: usize,
    pub spacing_v: f64,
    pub spacing_h: f64,
    pub origin_v: f64,
    pub origin_h: f64,
    pub table_points: usize,
    pub weights: [f32; 5],
    pub gains: Vec<f32>,
}

pub fn parse_gain_table_map(bytes: &[u8]) -> Option<GainTableMap> {
    [false, true]
        .into_iter()
        .find_map(|little_endian| parse_gain_table_map_with(bytes, little_endian))
}

fn parse_gain_table_map_with(bytes: &[u8], little_endian: bool) -> Option<GainTableMap> {
    let mut r = Reader {
        bytes,
        pos: 0,
        little_endian,
    };
    let mut map = GainTableMap {
        points_v: r.u32()? as usize,
        points_h: r.u32()? as usize,
        spacing_v: r.f64()?,
        spacing_h: r.f64()?,
        origin_v: r.f64()?,
        origin_h: r.f64()?,
        table_points: r.u32()? as usize,
        weights: [r.f32()?, r.f32()?, r.f32()?, r.f32()?, r.f32()?],
        gains: Vec::new(),
    };
    let count = map
        .points_v
        .checked_mul(map.points_h)?
        .checked_mul(map.table_points)?;
    if count == 0
        || count > 1 << 24
        || map.table_points < 2
        || bytes.len() != r.pos.checked_add(count.checked_mul(4)?)?
    {
        return None;
    }
    map.gains = (0..count).map(|_| r.f32()).collect::<Option<_>>()?;
    Some(map)
}

/// Applies a gain table map to an RGB image (linear, white at 1). The table
/// is indexed after the file's BaselineExposure (`baseline_ev`), as Camera
/// Raw does: ProRAW (−0.30 EV) went from dE 1.59 to 0.73 against it.
pub fn apply_gain_table_map(
    map: &GainTableMap,
    pixels: &mut [[f32; 3]],
    width: usize,
    height: usize,
    baseline_ev: f32,
) {
    if pixels.len() != width * height || width == 0 || height == 0 {
        return;
    }
    let n = map.table_points;
    let columns: Vec<(usize, usize, f32)> = (0..width)
        .map(|x| {
            map_position(
                (x as f64 + 0.5) / width as f64,
                map.origin_h,
                map.spacing_h,
                map.points_h,
            )
        })
        .collect();
    let exposure = 2f32.powf(baseline_ev);
    let w = map.weights.map(|weight| weight * exposure);
    pixels
        .par_chunks_exact_mut(width)
        .enumerate()
        .for_each(|(y, row)| {
            let (v0, v1, tv) = map_position(
                (y as f64 + 0.5) / height as f64,
                map.origin_v,
                map.spacing_v,
                map.points_v,
            );
            for (pixel, &(h0, h1, th)) in row.iter_mut().zip(&columns) {
                let [r, g, b] = *pixel;
                let value = (w[0] * r
                    + w[1] * g
                    + w[2] * b
                    + w[3] * r.min(g).min(b)
                    + w[4] * r.max(g).max(b))
                .clamp(0.0, 1.0);
                let position = value * (n - 1) as f32;
                let index = (position as usize).min(n - 2);
                let t = position - index as f32;
                let at = |row: usize, col: usize| {
                    let base = (row * map.points_h + col) * n;
                    map.gains[base + index]
                        + (map.gains[base + index + 1] - map.gains[base + index]) * t
                };
                let top = at(v0, h0) + (at(v0, h1) - at(v0, h0)) * th;
                let bottom = at(v1, h0) + (at(v1, h1) - at(v1, h0)) * th;
                let gain = top + (bottom - top) * tv;
                for c in pixel.iter_mut() {
                    *c *= gain;
                }
            }
        });
}

/// Normalisation radius shared by the radial opcodes: the distance from the
/// optical centre to the farthest stage-3 corner.
fn radial_frame(centre: (f64, f64), frame: Stage3Frame) -> (f64, f64, f64) {
    let cx = centre.0 * frame.width;
    let cy = centre.1 * frame.height;
    let dx = cx.max(frame.width - cx);
    let dy = cy.max(frame.height - cy);
    (cx, cy, (dx * dx + dy * dy).sqrt().max(1e-9))
}

fn apply_vignette(
    vignette: &FixVignetteRadial,
    pixels: &mut [[f32; 3]],
    width: usize,
    frame: Stage3Frame,
) {
    let (cx, cy, radius) = radial_frame(vignette.centre, frame);
    let k = vignette.k;
    pixels
        .par_chunks_exact_mut(width)
        .enumerate()
        .for_each(|(y, row)| {
            let dy = (y as f64 + frame.top + 0.5 - cy) / radius;
            for (x, pixel) in row.iter_mut().enumerate() {
                let dx = (x as f64 + frame.left + 0.5 - cx) / radius;
                let r2 = dx * dx + dy * dy;
                let gain = 1.0 + r2 * (k[0] + r2 * (k[1] + r2 * (k[2] + r2 * (k[3] + r2 * k[4]))));
                let gain = gain as f32;
                for value in pixel.iter_mut() {
                    *value *= gain;
                }
            }
        });
}

fn sample_bilinear(
    pixels: &[[f32; 3]],
    width: usize,
    height: usize,
    x: f64,
    y: f64,
    c: usize,
) -> f32 {
    let fx = (x - 0.5).clamp(0.0, (width - 1) as f64);
    let fy = (y - 0.5).clamp(0.0, (height - 1) as f64);
    let (x0, y0) = (fx.floor() as usize, fy.floor() as usize);
    let (x1, y1) = ((x0 + 1).min(width - 1), (y0 + 1).min(height - 1));
    let (tx, ty) = ((fx - x0 as f64) as f32, (fy - y0 as f64) as f32);
    let at = |xx: usize, yy: usize| pixels[yy * width + xx][c];
    let top = at(x0, y0) + (at(x1, y0) - at(x0, y0)) * tx;
    let bottom = at(x0, y1) + (at(x1, y1) - at(x0, y1)) * tx;
    top + (bottom - top) * ty
}

/// Source position (developed pixels) for a destination pixel centre.
fn warp_source(k: &[f64; 6], cx: f64, cy: f64, radius: f64, x: f64, y: f64) -> (f64, f64) {
    let dx = (x - cx) / radius;
    let dy = (y - cy) / radius;
    let r2 = dx * dx + dy * dy;
    let f = k[0] + r2 * (k[1] + r2 * (k[2] + r2 * k[3]));
    let tx = k[4] * 2.0 * dx * dy + k[5] * (r2 + 2.0 * dx * dx);
    let ty = k[5] * 2.0 * dx * dy + k[4] * (r2 + 2.0 * dy * dy);
    (cx + radius * (f * dx + tx), cy + radius * (f * dy + ty))
}

fn apply_warp(
    warp: &WarpRectilinear,
    pixels: &mut [[f32; 3]],
    width: usize,
    height: usize,
    frame: Stage3Frame,
) -> bool {
    if warp.is_identity() {
        return true;
    }
    let (cx, cy, radius) = radial_frame(warp.centre, frame);
    // Work in developed coordinates: shift the centre by the crop offset.
    let (cx, cy) = (cx - frame.left, cy - frame.top);
    let source = pixels.to_vec();
    let plane_coefficients: Vec<[f64; 6]> = (0..3)
        .map(|c| warp.coefficients[c.min(warp.coefficients.len() - 1)])
        .collect();
    pixels
        .par_chunks_exact_mut(width)
        .enumerate()
        .for_each(|(y, row)| {
            let yc = y as f64 + 0.5;
            for (x, pixel) in row.iter_mut().enumerate() {
                let xc = x as f64 + 0.5;
                for (c, k) in plane_coefficients.iter().enumerate() {
                    let (sx, sy) = warp_source(k, cx, cy, radius, xc, yc);
                    pixel[c] = sample_bilinear(&source, width, height, sx, sy, c);
                }
            }
        });
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(width: f64, height: f64) -> Stage3Frame {
        Stage3Frame {
            width,
            height,
            left: 0.0,
            top: 0.0,
            scale: 1.0,
        }
    }

    fn encode(opcodes: &[(u32, Vec<u8>)]) -> Vec<u8> {
        let mut out = (opcodes.len() as u32).to_be_bytes().to_vec();
        for (id, body) in opcodes {
            for value in [*id, 0x0103_0000, 0, body.len() as u32] {
                out.extend(value.to_be_bytes());
            }
            out.extend(body);
        }
        out
    }

    fn gain_map_body(gains: &[f32], points: u32, planes: u32) -> Vec<u8> {
        let mut body = Vec::new();
        for value in [0u32, 0, 4, 4, 0, planes, 1, 1, points, points] {
            body.extend(value.to_be_bytes());
        }
        let spacing = 1.0 / (points - 1) as f64;
        for value in [spacing, spacing, 0.0, 0.0] {
            body.extend(value.to_be_bytes());
        }
        body.extend(planes.to_be_bytes());
        for gain in gains {
            body.extend(gain.to_be_bytes());
        }
        body
    }

    #[test]
    fn map_polynomial_acts_on_normalised_values_of_its_planes() {
        // One opcode: area 0..1 × 0..2, planes 0-1 of 3, v ↦ 0.5 v + 0.5 v².
        let mut body = Vec::new();
        for v in [0u32, 0, 1, 2, 0, 2, 1, 1, 2] {
            body.extend(v.to_be_bytes());
        }
        for c in [0.0f64, 0.5, 0.5] {
            body.extend(c.to_be_bytes());
        }
        let mut list = Vec::new();
        for v in [1u32, MAP_POLYNOMIAL, 0x0103_0000, 0, body.len() as u32] {
            list.extend(v.to_be_bytes());
        }
        list.extend(&body);
        let opcodes = parse_opcode_list(&list);
        assert!(matches!(&opcodes[..], [Opcode::MapPolynomial(m)] if m.coefficients.len() == 3));
        // Black 10, white 110: 60 is 0.5 → 0.375 → 47.5; plane 2 untouched.
        let mut data = vec![60.0f32; 2 * 3];
        let n = apply_map_polynomials(&opcodes, &mut data, 2, 3, &[10.0], &[110.0]);
        assert_eq!(n, 1);
        assert_eq!(data, vec![47.5, 47.5, 60.0, 47.5, 47.5, 60.0]);
    }

    #[test]
    fn parses_gain_map_and_identity_warp() {
        let gains: Vec<f32> = (0..2 * 2 * 3).map(|i| 1.0 + i as f32).collect();
        let mut warp = 3u32.to_be_bytes().to_vec();
        for _ in 0..3 {
            for value in [1.0f64, 0.0, 0.0, 0.0, 0.0, 0.0] {
                warp.extend(value.to_be_bytes());
            }
        }
        warp.extend(0.5f64.to_be_bytes());
        warp.extend(0.5f64.to_be_bytes());
        let opcodes = parse_opcode_list(&encode(&[
            (GAIN_MAP, gain_map_body(&gains, 2, 3)),
            (42, vec![1, 2, 3]),
            (WARP_RECTILINEAR, warp),
        ]));
        assert_eq!(opcodes.len(), 2);
        let Opcode::GainMap(map) = &opcodes[0] else {
            panic!("expected gain map");
        };
        assert_eq!((map.points_v, map.points_h, map.map_planes), (2, 2, 3));
        assert_eq!(map.gains, gains);
        let Opcode::WarpRectilinear(warp) = &opcodes[1] else {
            panic!("expected warp");
        };
        assert!(warp.is_identity());
        // Truncated input yields what was readable.
        assert!(parse_opcode_list(&[0, 0, 0, 5, 0]).is_empty());
    }

    #[test]
    fn gain_map_scales_camera_planes_through_the_output_matrix() {
        // Uniform per-plane gains (2, 1, 1) on camera RGB.
        let gains = [2.0f32, 1.0, 1.0].repeat(4);
        let opcodes = parse_opcode_list(&encode(&[(GAIN_MAP, gain_map_body(&gains, 2, 3))]));
        let m = [
            [1.2f32, -0.15, -0.05],
            [-0.1, 1.15, -0.05],
            [0.0, -0.2, 1.2],
        ];
        let camera = [0.2f32, 0.3, 0.1];
        let output = mul3(&m, camera);
        let mut pixels = vec![output; 16];
        assert_eq!(
            apply_opcodes(&opcodes, &mut pixels, 4, 4, frame(4.0, 4.0), Some(m)),
            1
        );
        let expected = mul3(&m, [0.4, 0.3, 0.1]);
        for pixel in &pixels {
            for c in 0..3 {
                assert!(
                    (pixel[c] - expected[c]).abs() < 1e-5,
                    "{pixel:?} vs {expected:?}"
                );
            }
        }
    }

    #[test]
    fn gain_map_interpolates_between_points() {
        // 2x2 single-plane map: 1 at the top, 3 at the bottom.
        let opcodes = parse_opcode_list(&encode(&[(
            GAIN_MAP,
            gain_map_body(&[1.0, 1.0, 3.0, 3.0], 2, 1),
        )]));
        let mut pixels = vec![[1.0f32; 3]; 16];
        apply_opcodes(&opcodes, &mut pixels, 4, 4, frame(4.0, 4.0), None);
        // Pixel centres at 0.125, 0.375, 0.625, 0.875 of the height.
        for (row, expected) in [1.25f32, 1.75, 2.25, 2.75].iter().enumerate() {
            assert!((pixels[row * 4][0] - expected).abs() < 1e-5);
            // Planes = 1 corrects only the first plane.
            assert_eq!(pixels[row * 4][1], 1.0);
        }
    }

    #[test]
    fn cfa_gain_maps_touch_only_their_bayer_position() {
        // One map for the (row 0, column 1) Bayer position: gain 2 everywhere.
        let mut body = Vec::new();
        for value in [0u32, 1, 4, 4, 0, 1, 2, 2, 2, 2] {
            body.extend(value.to_be_bytes());
        }
        for value in [1.0f64, 1.0, 0.0, 0.0] {
            body.extend(value.to_be_bytes());
        }
        body.extend(1u32.to_be_bytes());
        for _ in 0..4 {
            body.extend(2.0f32.to_be_bytes());
        }
        let opcodes = parse_opcode_list(&encode(&[(GAIN_MAP, body)]));
        // 6x6 raw with a one-pixel masked border: active area 4x4 at (1, 1).
        let mut data = vec![1.0f32; 36];
        assert_eq!(apply_cfa_gain_maps(&opcodes, &mut data, 6, (1, 1, 4, 4)), 1);
        for y in 0..6 {
            for x in 0..6 {
                let active = (1..5).contains(&x) && (1..5).contains(&y);
                let expected = if active && (y - 1) % 2 == 0 && (x - 1) % 2 == 1 {
                    2.0
                } else {
                    1.0
                };
                assert_eq!(data[y * 6 + x], expected, "pixel {x},{y}");
            }
        }
    }

    #[test]
    fn gain_table_map_follows_the_weighted_value() {
        // 1x1 map, 2 table points: gain 3 at value 0, gain 1 at value 1.
        let mut body = Vec::new();
        for value in [1u32, 1] {
            body.extend(value.to_be_bytes());
        }
        for value in [1.0f64, 1.0, 0.0, 0.0] {
            body.extend(value.to_be_bytes());
        }
        body.extend(2u32.to_be_bytes());
        for weight in [0.0f32, 0.0, 0.0, 0.0, 1.0] {
            body.extend(weight.to_be_bytes());
        }
        for gain in [3.0f32, 1.0] {
            body.extend(gain.to_be_bytes());
        }
        let map = parse_gain_table_map(&body).unwrap();
        let mut pixels = vec![[0.5f32, 0.25, 0.1]];
        apply_gain_table_map(&map, &mut pixels, 1, 1, 0.0);
        // value = max = 0.5 -> gain 2.
        assert_eq!(pixels[0], [1.0, 0.5, 0.2]);
        // One stop of baseline exposure doubles the value: 0.25 -> 0.5.
        let mut pixels = vec![[0.25f32, 0.125, 0.05]];
        apply_gain_table_map(&map, &mut pixels, 1, 1, 1.0);
        assert_eq!(pixels[0], [0.5, 0.25, 0.1]);
        assert!(parse_gain_table_map(&body[..body.len() - 2]).is_none());
        // The same map in little-endian order parses identically.
        let mut little = Vec::new();
        for value in [1u32, 1] {
            little.extend(value.to_le_bytes());
        }
        for value in [1.0f64, 1.0, 0.0, 0.0] {
            little.extend(value.to_le_bytes());
        }
        little.extend(2u32.to_le_bytes());
        for weight in [0.0f32, 0.0, 0.0, 0.0, 1.0] {
            little.extend(weight.to_le_bytes());
        }
        for gain in [3.0f32, 1.0] {
            little.extend(gain.to_le_bytes());
        }
        assert_eq!(parse_gain_table_map(&little), Some(map));
    }

    #[test]
    fn warp_follows_the_dng_radial_model() {
        // Barrel model with kr1: the source radius grows by (1 + kr1 r²).
        let k = [1.0, 0.1, 0.0, 0.0, 0.0, 0.0];
        let (cx, cy, radius) = radial_frame((0.5, 0.5), frame(100.0, 100.0));
        let (sx, sy) = warp_source(&k, cx, cy, radius, 100.0, 50.0);
        let r = 50.0 / radius;
        assert!((sx - (50.0 + 50.0 * (1.0 + 0.1 * r * r))).abs() < 1e-9);
        assert!((sy - 50.0).abs() < 1e-9);
        // Identity keeps the image untouched.
        let warp = WarpRectilinear {
            coefficients: vec![[1.0, 0.0, 0.0, 0.0, 0.0, 0.0]],
            centre: (0.5, 0.5),
        };
        let mut pixels: Vec<[f32; 3]> = (0..16).map(|i| [i as f32; 3]).collect();
        let before = pixels.clone();
        assert!(apply_warp(&warp, &mut pixels, 4, 4, frame(4.0, 4.0)));
        assert_eq!(pixels, before);
    }

    #[test]
    fn radial_vignette_brightens_corners_only() {
        let vignette = FixVignetteRadial {
            k: [1.0, 0.0, 0.0, 0.0, 0.0],
            centre: (0.5, 0.5),
        };
        let mut pixels = vec![[1.0f32; 3]; 9];
        apply_vignette(&vignette, &mut pixels, 3, frame(3.0, 3.0));
        assert!((pixels[4][0] - 1.0).abs() < 1e-6);
        // Corner pixel centre sits at (1/3 of the corner radius)·2 ... r² = 4/9.
        assert!((pixels[0][0] - (1.0 + 4.0 / 9.0)).abs() < 1e-5);
    }
}
