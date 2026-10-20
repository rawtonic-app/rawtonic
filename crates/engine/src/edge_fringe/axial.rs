//! Version 4's first stage: chromatic aberration across strong edges,
//! mostly axial (purple or blue fringes on dark branches against the sky,
//! a green or yellow rim on the dark side of a bright edge), on noisy
//! photos too.
//!
//! Lenses focus colours at different depths, so one channel of an edge is
//! blurred wider or set off from the others: the dark side picks up light
//! of that colour from the bright side (purple fringing near highlights),
//! or keeps too little of it. The fringe can be several pixels wide, wider
//! than the windows of the luminance and colour-line stages. Blur alone
//! (the same in every channel) mixes the two sides of an edge in linear
//! light: every pixel between them is a mix of the two side colours at its
//! own luminance. So each pixel near a strong edge looks along the
//! luminance gradient for the edge's dark and bright sides (up to 8 px
//! each way), and takes the colour of the side mix at its own luminance,
//! keeping that luminance.
//!
//! What protects real colour:
//! - Chromatic aberration only redistributes each channel between its two
//!   side values; a pixel with a channel well beyond both is a third
//!   colour (a red stroke along an edge has less green and blue than
//!   either side would give) and keeps its colour, as in the colour-line
//!   stage.
//! - A band of a third colour between two sides shows as a second step in
//!   the luminance along the gradient; one blurred step has one. A pixel
//!   whose path between the sides has two steps keeps its colour (a light
//!   leaf between a dark one and the sky, a printed stroke).
//! - Only colour away from the side mix in the purple-to-blue or the
//!   yellow-to-green families moves fully (the hues a lens's axial
//!   chromatic aberration gives); red, orange or cyan deviations move only
//!   when they are small (chroma noise along the edge).
//! - Only edges of more than about two stops with a luminance step well
//!   above the photo's noise count, and a pixel never gets more colour
//!   than it had.
//!
//! Each pixel's change depends only on the pixels within [`reach`] of it,
//! so tiles, bands and regions give the same result as the whole image.
//! (notes: reference/edge-fringes.md "Version 4")

use rayon::prelude::*;

/// Gaussian of the luminance whose gradient gives the edge's direction and
/// along which the sides are searched (full-resolution pixels).
const DIRECTION_SIGMA: f32 = 1.5;
/// Gaussian of the colours the sides are read from (averages noise).
const SIDE_SIGMA: f32 = 1.0;
/// Gaussian of the luminance profile of the two-step test.
const PROFILE_SIGMA: f32 = 0.7;
/// How far each side is searched along the gradient, and how far beyond
/// the darkest and brightest point its colour is read.
const SEARCH: f32 = 8.0;
const PUSH: f32 = 1.0;
/// Luminance step between the sides (square-root units) from which a
/// pixel starts to change, the span to full change, and the multiple of
/// the photo's luminance noise the start is at least.
const STEP_LOW: f32 = 0.1;
const STEP_SPAN: f32 = 0.08;
const STEP_NOISE: f32 = 6.0;
/// Contrast between the sides (stops of linear luminance): from no change
/// to full change.
const STOPS_LOW: f32 = 1.5;
const STOPS_HIGH: f32 = 2.5;
/// A channel may lie this many noise standard deviations beyond its side
/// values before it counts as beyond them, then up to `BEYOND_FULL` of the
/// sides' largest channel difference and no change from `BEYOND_NONE`.
const NOISE_TOLERANCE: f32 = 3.0;
const BEYOND_FULL: f32 = 0.1;
const BEYOND_NONE: f32 = 0.25;
/// Oklab hue ranges (degrees) of the deviations from the side mix that move
/// fully, and the shoulder beyond them.
const PURPLE: (f32, f32) = (255.0, 355.0);
const GREEN: (f32, f32) = (95.0, 200.0);
const HUE_SHOULDER: f32 = 15.0;
/// Oklab chroma of a deviation of any other hue that still moves fully,
/// and from which not at all, in multiples of the photo's luminance noise:
/// chroma noise along the edge, which the colour noise reduction would
/// leave as blotches beside it.
const SMALL_FULL: f32 = 1.3;
const SMALL_NONE: f32 = 2.6;
/// The two-step test: depth of the dip between two luminance steps along
/// the path (relative to the steeper step) from which a pixel starts to
/// keep its colour, and from which it keeps it fully.
const STEPS_FULL: f32 = 0.5;
const STEPS_NONE: f32 = 0.8;
/// Tile size of the parallel work (rows, columns), plus the halo each
/// tile reads around it.
const TILE_ROWS: usize = 128;
const TILE_COLS: usize = 256;

/// The distances of the stage on an image at `scale` of the full
/// resolution.
#[derive(Clone, Copy, Debug)]
struct Geometry {
    direction: Kernel,
    side: Kernel,
    profile: Kernel,
    search: usize,
    push: f32,
}

#[derive(Clone, Debug, Copy)]
struct Kernel {
    radius: usize,
    sigma: f32,
}

impl Kernel {
    fn new(sigma: f32) -> Self {
        Self {
            radius: ((3.0 * sigma).ceil() as usize).max(1),
            sigma,
        }
    }

    fn weights(&self) -> Vec<f32> {
        let r = self.radius as isize;
        let raw: Vec<f32> = (-r..=r)
            .map(|i| (-((i * i) as f32) / (2.0 * self.sigma * self.sigma)).exp())
            .collect();
        let sum: f32 = raw.iter().sum();
        raw.iter().map(|v| v / sum).collect()
    }
}

impl Geometry {
    fn at(scale: f32) -> Self {
        let scale = scale.clamp(0.0, 1.0);
        Self {
            direction: Kernel::new(DIRECTION_SIGMA * scale),
            side: Kernel::new(SIDE_SIGMA * scale),
            profile: Kernel::new(PROFILE_SIGMA * scale),
            search: ((SEARCH * scale).round() as usize).max(2),
            push: PUSH * scale,
        }
    }

    /// How far from a pixel the stage reads: the 3 × 3 weight median, the
    /// gradient, the side search with bilinear reads, and each read's
    /// Gaussian.
    fn reach(&self) -> usize {
        let far = self.search + self.push.ceil() as usize + 2;
        2 + (far + self.direction.radius)
            .max(far + self.side.radius)
            .max(far + self.profile.radius)
    }
}

/// How far the stage reads around a pixel on an image at `scale`.
pub fn reach(scale: f32) -> usize {
    Geometry::at(scale).reach()
}

fn smooth(v: f32, lo: f32, hi: f32) -> f32 {
    let t = ((v - lo) / (hi - lo)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn lum(c: [f32; 3]) -> f32 {
    0.25 * (c[0] + 2.0 * c[1] + c[2])
}

fn oklab(c: [f32; 3]) -> [f32; 3] {
    let l = 0.412_221_47 * c[0] + 0.536_332_55 * c[1] + 0.051_445_995 * c[2];
    let m = 0.211_903_5 * c[0] + 0.680_699_5 * c[1] + 0.107_396_96 * c[2];
    let s = 0.088_302_46 * c[0] + 0.281_718_85 * c[1] + 0.629_978_7 * c[2];
    let (l, m, s) = (l.max(0.0).cbrt(), m.max(0.0).cbrt(), s.max(0.0).cbrt());
    [
        0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s,
        1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
        0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s,
    ]
}

/// Membership (1 inside, 0 beyond the shoulder) of `hue` in `range`.
fn in_hue(hue: f32, (lo, hi): (f32, f32)) -> f32 {
    let half = 0.5 * (hi - lo);
    let off = ((hue - 0.5 * (lo + hi)).rem_euclid(360.0) + 180.0).rem_euclid(360.0) - 180.0;
    1.0 - smooth(off.abs(), half, half + HUE_SHOULDER)
}

/// A rectangle of the image, its Gaussian-filtered luminance (square-root
/// units, two widths) and colours, valid wherever a pixel of the tile and
/// its ring reads them; sampled in image coordinates.
struct Planes {
    x0: usize,
    y0: usize,
    w: usize,
    direction: Vec<f32>,
    profile: Vec<f32>,
    colour: [Vec<f32>; 3],
}

impl Planes {
    /// Bilinear read of `plane` at the offset (`ox`, `oy`) from the pixel
    /// (`x`, `y`), clamped to the image (`width × height`), which the
    /// planes cover there. The interpolation weights come from the offset
    /// alone, so a region of the image reads the same values as the whole
    /// image (an absolute position would round differently at another
    /// origin).
    #[allow(clippy::too_many_arguments)]
    fn read(
        &self,
        plane: &[f32],
        width: usize,
        height: usize,
        (x, y): (usize, usize),
        ox: f32,
        oy: f32,
    ) -> f32 {
        let axis = |p: usize, o: f32, len: usize| -> (usize, usize, f32) {
            let whole = o.floor();
            let a = p as isize + whole as isize;
            if a < 0 {
                (0, 0, 0.0)
            } else if a as usize >= len - 1 {
                (len - 1, len - 1, 0.0)
            } else {
                (a as usize, a as usize + 1, o - whole)
            }
        };
        let (xa, xb, fx) = axis(x, ox, width);
        let (ya, yb, fy) = axis(y, oy, height);
        let at = |px: usize, py: usize| plane[(py - self.y0) * self.w + px - self.x0];
        let top = at(xa, ya) * (1.0 - fx) + at(xb, ya) * fx;
        let bottom = at(xa, yb) * (1.0 - fx) + at(xb, yb) * fx;
        top * (1.0 - fy) + bottom * fy
    }

    /// [`Planes::read`] where the four pixels read lie inside the image:
    /// the same arithmetic without the clamps.
    #[inline(always)]
    fn read_inside(&self, plane: &[f32], (x, y): (usize, usize), ox: f32, oy: f32) -> f32 {
        let (wx, wy) = (ox.floor(), oy.floor());
        let (fx, fy) = (ox - wx, oy - wy);
        let xa = (x as isize + wx as isize) as usize;
        let ya = (y as isize + wy as isize) as usize;
        let i = (ya - self.y0) * self.w + xa - self.x0;
        let top = plane[i] * (1.0 - fx) + plane[i + 1] * fx;
        let bottom = plane[i + self.w] * (1.0 - fx) + plane[i + self.w + 1] * fx;
        top * (1.0 - fy) + bottom * fy
    }
}

/// The square-root luminance and the three channels of the rectangle
/// `x0.., y0..` of `w × h` and `pad` pixels around it, read with the
/// image's edges (`width × height`) repeated, as the whole image repeats
/// them.
fn padded(
    data: &[f32],
    stride: usize,
    width: usize,
    height: usize,
    (x0, y0, w, h): (usize, usize, usize, usize),
    pad: usize,
) -> [Vec<f32>; 4] {
    let pw = w + 2 * pad;
    let len = pw * (h + 2 * pad);
    let mut out: [Vec<f32>; 4] = std::array::from_fn(|_| vec![0.0f32; len]);
    let columns: Vec<usize> = (0..pw)
        .map(|i| (x0 as isize + i as isize - pad as isize).clamp(0, width as isize - 1) as usize)
        .collect();
    for j in 0..h + 2 * pad {
        let y = (y0 as isize + j as isize - pad as isize).clamp(0, height as isize - 1) as usize;
        let row = &data[y * width * stride..(y + 1) * width * stride];
        for (i, &x) in columns.iter().enumerate() {
            let p = &row[x * stride..x * stride + 3];
            let at = j * pw + i;
            out[0][at] = lum([p[0], p[1], p[2]]).max(0.0).sqrt();
            out[1][at] = p[0];
            out[2][at] = p[1];
            out[3][at] = p[2];
        }
    }
    out
}

/// The Gaussian `kernel` of the `w × h` rectangle whose values with `pad`
/// pixels around it are `source` (`kernel.radius <= pad`). Each output
/// adds its taps in kernel order.
fn gaussian(source: &[f32], w: usize, h: usize, pad: usize, kernel: Kernel) -> Vec<f32> {
    let weights = kernel.weights();
    let r = kernel.radius;
    let pw = w + 2 * pad;
    // Rows pad - r .. pad + h + r of the source, filtered along x.
    let rows = h + 2 * r;
    let mut across = vec![0.0f32; rows * w];
    for (e, out) in across.chunks_exact_mut(w).enumerate() {
        let row = &source[(pad - r + e) * pw..(pad - r + e + 1) * pw];
        for (k, weight) in weights.iter().enumerate() {
            let taps = &row[pad - r + k..pad - r + k + w];
            for (o, v) in out.iter_mut().zip(taps) {
                *o += weight * v;
            }
        }
    }
    let mut out = vec![0.0f32; w * h];
    for (y, row) in out.chunks_exact_mut(w).enumerate() {
        for (k, weight) in weights.iter().enumerate() {
            let taps = &across[(y + k) * w..(y + k + 1) * w];
            for (o, v) in row.iter_mut().zip(taps) {
                *o += weight * v;
            }
        }
    }
    out
}

/// Running extreme of `values` over `x - r ..= x + r` (clipped) into `out`
/// (van Herk / Gil–Werman): `pick` is `f32::min` or `f32::max` and
/// `neutral` the value it never picks over another.
fn running(
    values: &[f32],
    r: usize,
    pick: fn(f32, f32) -> f32,
    neutral: f32,
    out: &mut [f32],
    scratch: &mut Vec<f32>,
) {
    let n = values.len();
    let k = 2 * r + 1;
    // Over the values padded with `neutral` by r on each side: extremes
    // from each block's start (prefix) and to its end (suffix).
    let blocks = (n + 2 * r).div_ceil(k) * k;
    scratch.clear();
    scratch.resize(2 * blocks, neutral);
    let (prefix, suffix) = scratch.split_at_mut(blocks);
    let at = |i: usize| {
        if i >= r && i < r + n {
            values[i - r]
        } else {
            neutral
        }
    };
    for b in (0..blocks).step_by(k) {
        let mut acc = neutral;
        for (i, p) in prefix[b..b + k].iter_mut().enumerate() {
            acc = pick(acc, at(b + i));
            *p = acc;
        }
        let mut acc = neutral;
        for (i, q) in suffix[b..b + k].iter_mut().enumerate().rev() {
            acc = pick(acc, at(b + i));
            *q = acc;
        }
    }
    // The window of padded positions x ..= x + 2r.
    for (x, o) in out.iter_mut().enumerate() {
        *o = pick(suffix[x], prefix[x + 2 * r]);
    }
}

/// Smallest and largest value of `plane` (`w × h`) in the square of
/// radius `r` around each pixel (clipped to the plane).
fn window_extremes(plane: &[f32], w: usize, h: usize, r: usize) -> (Vec<f32>, Vec<f32>) {
    let mut scratch = Vec::new();
    let mut lo = vec![0.0f32; w * h];
    let mut hi = vec![0.0f32; w * h];
    for y in 0..h {
        let row = &plane[y * w..(y + 1) * w];
        let span = y * w..(y + 1) * w;
        running(
            row,
            r,
            f32::min,
            f32::INFINITY,
            &mut lo[span.clone()],
            &mut scratch,
        );
        running(
            row,
            r,
            f32::max,
            f32::NEG_INFINITY,
            &mut hi[span],
            &mut scratch,
        );
    }
    let mut column = vec![0.0f32; h];
    let mut extreme = vec![0.0f32; h];
    for (plane, pick, neutral) in [
        (&mut lo, f32::min as fn(f32, f32) -> f32, f32::INFINITY),
        (&mut hi, f32::max, f32::NEG_INFINITY),
    ] {
        for x in 0..w {
            for y in 0..h {
                column[y] = plane[y * w + x];
            }
            running(&column, r, pick, neutral, &mut extreme, &mut scratch);
            for y in 0..h {
                plane[y * w + x] = extreme[y];
            }
        }
    }
    (lo, hi)
}

/// The stage in place on `w × h` pixels of `stride` floats (RGB first),
/// linear, at `scale` of the full resolution; `noise` is the image's own
/// luminance noise (standard deviation, square-root units).
pub fn apply(data: &mut [f32], w: usize, h: usize, stride: usize, noise: f32, scale: f32) {
    apply_in_tiles(data, (w, h, stride), noise, scale, (TILE_ROWS, TILE_COLS));
}

/// [`apply`] in tiles of `tile_rows × tile_cols`.
fn apply_in_tiles(
    data: &mut [f32],
    (w, h, stride): (usize, usize, usize),
    noise: f32,
    scale: f32,
    (tile_rows, tile_cols): (usize, usize),
) {
    if w < 3 || h < 3 {
        return;
    }
    let geometry = Geometry::at(scale);
    let source: &[f32] = data;
    let across = w.div_ceil(tile_cols);
    let changes: Vec<Vec<(usize, [f32; 3])>> = (0..h.div_ceil(tile_rows) * across)
        .into_par_iter()
        .map(|tile| {
            let (band, column) = (tile / across, tile % across);
            let rows = band * tile_rows..((band + 1) * tile_rows).min(h);
            let columns = column * tile_cols..((column + 1) * tile_cols).min(w);
            tile_changes(source, w, h, stride, noise, &geometry, rows, columns)
        })
        .collect();
    for (i, px) in changes.into_iter().flatten() {
        data[i * stride..i * stride + 3].copy_from_slice(&px);
    }
}

/// A pixel's target colour and how far it moves there.
#[derive(Clone, Copy)]
struct Move {
    weight: f32,
    /// The weight without the test of channels beyond the sides: as far
    /// as moving neighbours may take the pixel along.
    cap: f32,
    target: [f32; 3],
}

#[allow(clippy::too_many_arguments)]
fn tile_changes(
    data: &[f32],
    width: usize,
    height: usize,
    stride: usize,
    noise: f32,
    geometry: &Geometry,
    rows: std::ops::Range<usize>,
    columns: std::ops::Range<usize>,
) -> Vec<(usize, [f32; 3])> {
    // The tile and a one-pixel ring (the weight median reads it).
    let ring_x0 = columns.start.saturating_sub(1);
    let ring_y0 = rows.start.saturating_sub(1);
    let ring_x1 = (columns.end + 1).min(width);
    let ring_y1 = (rows.end + 1).min(height);
    // Planes around the ring, as far as its pixels read.
    let halo = geometry.reach();
    let px0 = columns.start.saturating_sub(halo);
    let py0 = rows.start.saturating_sub(halo);
    let pw = (columns.end + halo).min(width) - px0;
    let ph = (rows.end + halo).min(height) - py0;
    let rect = (px0, py0, pw, ph);
    let pixel = |x: usize, y: usize| &data[(y * width + x) * stride..(y * width + x) * stride + 3];
    let pad = geometry
        .direction
        .radius
        .max(geometry.side.radius)
        .max(geometry.profile.radius);
    let [roots, red, green, blue] = padded(data, stride, width, height, rect, pad);
    let direction = gaussian(&roots, pw, ph, pad, geometry.direction);
    // Every luminance a pixel's side search reads lies within `search + 1`
    // of it; where those vary less than the edge test's start, the pixel
    // cannot change and is skipped (exactly: a necessary condition).
    let low = STEP_LOW.max(STEP_NOISE * noise);
    let (darkest, brightest) = window_extremes(&direction, pw, ph, geometry.search + 1);
    let rw = ring_x1 - ring_x0;
    let rh = ring_y1 - ring_y0;
    let at = |x: usize, y: usize| (y - py0) * pw + x - px0;
    let edge_near = |x: usize, y: usize| brightest[at(x, y)] - darkest[at(x, y)] >= low;
    let mut moves: Vec<Option<Move>> = vec![None; rw * rh];
    if !(ring_y0..ring_y1).any(|y| (ring_x0..ring_x1).any(|x| edge_near(x, y))) {
        return Vec::new();
    }
    let colour = |c: &[f32]| gaussian(c, pw, ph, pad, geometry.side);
    let planes = Planes {
        x0: px0,
        y0: py0,
        w: pw,
        profile: gaussian(&roots, pw, ph, pad, geometry.profile),
        direction,
        colour: [colour(&red), colour(&green), colour(&blue)],
    };
    for y in ring_y0..ring_y1 {
        for x in ring_x0..ring_x1 {
            if !edge_near(x, y) {
                continue;
            }
            moves[(y - ring_y0) * rw + x - ring_x0] =
                pixel_move(&planes, pixel(x, y), x, y, width, height, noise, geometry);
        }
    }
    let mut changes = Vec::new();
    for y in rows.clone() {
        for x in columns.clone() {
            let Some(own) = moves[(y - ring_y0) * rw + x - ring_x0] else {
                continue;
            };
            // A fringe pixel left out amid moved neighbours because a
            // channel overshoots both sides (at the fringe's peak) moves
            // with them; the hue, contrast and two-step tests still hold.
            let mut near = [0.0f32; 9];
            let mut n = 0;
            for ny in y.saturating_sub(1)..(y + 2).min(height) {
                for nx in x.saturating_sub(1)..(x + 2).min(width) {
                    near[n] = moves[(ny - ring_y0) * rw + nx - ring_x0].map_or(0.0, |m| m.weight);
                    n += 1;
                }
            }
            let near = &mut near[..n];
            near.sort_by(f32::total_cmp);
            let weight = own.weight.max(near[n / 2].min(own.cap));
            if weight <= 0.0 {
                continue;
            }
            let p = pixel(x, y);
            changes.push((
                y * width + x,
                std::array::from_fn(|c| p[c] + weight * (own.target[c] - p[c])),
            ));
        }
    }
    changes
}

/// Where the pixel `p` at (`x`, `y`) moves and how far; None away from
/// strong edges.
#[allow(clippy::too_many_arguments)]
fn pixel_move(
    planes: &Planes,
    p: &[f32],
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    noise: f32,
    geometry: &Geometry,
) -> Option<Move> {
    let p = [p[0], p[1], p[2]];
    if p.iter().any(|c| !c.is_finite()) {
        return None;
    }
    // Every read lies within `search + push + 2` of the pixel: away from
    // the image's edges the reads need no clamping.
    let margin = geometry.search + geometry.push.ceil() as usize + 2;
    let inside = x >= margin && y >= margin && x + margin < width && y + margin < height;
    let lum_at = |plane: &[f32], dx: f32, dy: f32| {
        if inside {
            planes.read_inside(plane, (x, y), dx, dy)
        } else {
            planes.read(plane, width, height, (x, y), dx, dy)
        }
    };
    let dir = &planes.direction;
    let gx = 0.5 * (lum_at(dir, 1.0, 0.0) - lum_at(dir, -1.0, 0.0));
    let gy = 0.5 * (lum_at(dir, 0.0, 1.0) - lum_at(dir, 0.0, -1.0));
    let g = gx.hypot(gy);
    // (NaN included.)
    if g.is_nan() || g <= 1e-5 {
        return None;
    }
    let (dx, dy) = (gx / g, gy / g);
    // The brightest point along the gradient and the darkest against it.
    let own = lum_at(dir, 0.0, 0.0);
    let (mut bright, mut to_bright) = (own, 0.0f32);
    let (mut dark, mut to_dark) = (own, 0.0f32);
    for k in 1..=geometry.search {
        let k = k as f32;
        let ahead = lum_at(dir, k * dx, k * dy);
        if ahead > bright {
            bright = ahead;
            to_bright = k;
        }
        let behind = lum_at(dir, -k * dx, -k * dy);
        if behind < dark {
            dark = behind;
            to_dark = k;
        }
    }
    let low = STEP_LOW.max(STEP_NOISE * noise);
    let edge = smooth(bright - dark, low, low + STEP_SPAN);
    if edge <= 0.0 {
        return None;
    }
    let side =
        |k: f32| -> [f32; 3] { std::array::from_fn(|c| lum_at(&planes.colour[c], k * dx, k * dy)) };
    let lit = side(to_bright + geometry.push);
    let shade = side(-(to_dark + geometry.push));
    let (l_lit, l_shade, l_own) = (lum(lit), lum(shade), lum(p));
    if (l_lit - l_shade).is_nan() || l_lit - l_shade <= 1e-6 {
        return None;
    }
    // The side mix at the pixel's luminance.
    let t = ((l_own - l_shade) / (l_lit - l_shade)).clamp(0.0, 1.0);
    let mix: [f32; 3] = std::array::from_fn(|c| shade[c] + t * (lit[c] - shade[c]));
    let span = (0..3)
        .map(|c| (lit[c] - shade[c]).abs())
        .fold(0.0f32, f32::max)
        .max(1e-6);
    let mut beyond = 0.0f32;
    for c in 0..3 {
        let tolerance = NOISE_TOLERANCE * 2.0 * p[c].max(0.0).sqrt() * noise;
        let lo = lit[c].min(shade[c]) - tolerance;
        let hi = lit[c].max(shade[c]) + tolerance;
        beyond = beyond.max((p[c] - hi).max(lo - p[c]).max(0.0) / span);
    }
    let between = 1.0 - smooth(beyond, BEYOND_FULL, BEYOND_NONE);
    let contrast = smooth(
        (l_lit.max(1e-6) / l_shade.max(1e-6)).log2(),
        STOPS_LOW,
        STOPS_HIGH,
    );
    // The mix's colour at the pixel's own luminance, never more colour
    // than the pixel had.
    let l_mix = lum(mix);
    let mut colour: [f32; 3] = std::array::from_fn(|c| mix[c] - l_mix);
    let norm = |v: &[f32; 3]| (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    let had = norm(&std::array::from_fn(|c| p[c] - l_own));
    let gets = norm(&colour);
    if gets > had && gets > 0.0 {
        for v in &mut colour {
            *v *= had / gets;
        }
    }
    let target: [f32; 3] = std::array::from_fn(|c| (l_own + colour[c]).max(0.0));
    // Which deviation: the hue families of axial aberration, or small.
    let (a, b) = (oklab(p), oklab(target));
    let (da, db) = (a[1] - b[1], a[2] - b[2]);
    let hue = db.atan2(da).to_degrees().rem_euclid(360.0);
    let small = if noise > 0.0 {
        1.0 - smooth(da.hypot(db), SMALL_FULL * noise, SMALL_NONE * noise)
    } else {
        0.0
    };
    let family = in_hue(hue, PURPLE).max(in_hue(hue, GREEN)).max(small);
    let weight = edge * between * contrast * family;
    let cap = edge * contrast * family;
    // The two-step test only where it can matter.
    let single = if cap > 0.0 {
        let dip = two_steps(
            planes,
            (x, y, inside),
            dx,
            dy,
            to_dark,
            to_bright,
            width,
            height,
        );
        1.0 - smooth(dip, STEPS_FULL, STEPS_NONE)
    } else {
        1.0
    };
    Some(Move {
        weight: weight * single,
        cap: cap * single,
        target,
    })
}

/// The dip between two luminance steps on the path from the dark side
/// (`to_dark` against the gradient) to the bright side (`to_bright` along
/// it), relative to the steeper step: 0 for a single blurred step.
#[allow(clippy::too_many_arguments)]
fn two_steps(
    planes: &Planes,
    (x, y, inside): (usize, usize, bool),
    dx: f32,
    dy: f32,
    to_dark: f32,
    to_bright: f32,
    width: usize,
    height: usize,
) -> f32 {
    let first = -(to_dark as i32) - 1;
    let last = to_bright as i32 + 1;
    let mut profile = [0.0f32; 64];
    let mut n = 0;
    for k in first..=last {
        let k = k as f32;
        profile[n] = if inside {
            planes.read_inside(&planes.profile, (x, y), k * dx, k * dy)
        } else {
            planes.read(&planes.profile, width, height, (x, y), k * dx, k * dy)
        };
        n += 1;
    }
    let steps = &profile[..n];
    let rise = |j: usize| steps[j + 1] - steps[j];
    let steepest = (0..n - 1).map(rise).fold(0.0f32, f32::max);
    if steepest <= 0.0 {
        return 0.0;
    }
    // Steepest rise after each position, from the end.
    let mut after = [0.0f32; 64];
    for j in (0..n - 1).rev() {
        after[j] = if j + 2 < n {
            rise(j + 1).max(after[j + 1])
        } else {
            0.0
        };
    }
    let (mut before, mut dip) = (0.0f32, 0.0f32);
    for (j, &later) in after[..n - 1].iter().enumerate() {
        dip = dip.max(before.min(later) - rise(j));
        before = before.max(rise(j));
    }
    dip / steepest
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Chroma of a linear RGB colour in Oklab.
    fn chroma(p: &[f32]) -> f32 {
        let lab = oklab([p[0], p[1], p[2]]);
        lab[1].hypot(lab[2])
    }

    /// Blur of a step at `edge` (bright to the right) with a Gaussian of
    /// `sigma`, between `dark` and `bright`.
    fn step(x: f32, edge: f32, sigma: f32, dark: f32, bright: f32) -> f32 {
        let t = (x - edge) / (sigma * std::f32::consts::SQRT_2);
        // Error function (Abramowitz–Stegun 7.1.26).
        let erf = |z: f32| {
            let s = z.signum();
            let z = z.abs();
            let t = 1.0 / (1.0 + 0.327_591_1 * z);
            let y = 1.0
                - (((((1.061_405_4 * t - 1.453_152_1) * t) + 1.421_413_7) * t - 0.284_496_74) * t
                    + 0.254_829_6)
                    * t
                    * (-z * z).exp();
            s * y
        };
        dark + (bright - dark) * 0.5 * (1.0 + erf(t))
    }

    /// A vertical edge between a dark neutral side and a bright neutral
    /// one whose channels are blurred with `sigmas` (axial aberration
    /// blurs one colour wider), plus noise of `noise` (square-root units).
    fn edge_image(w: usize, h: usize, sigmas: [f32; 3], noise: f32) -> Vec<f32> {
        let mut state = 0x9e37_79b9_u32;
        let mut uniform = move || {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state as f32 / u32::MAX as f32 - 0.5
        };
        let mut data = vec![0.0f32; w * h * 3];
        for y in 0..h {
            for x in 0..w {
                for c in 0..3 {
                    let v = step(x as f32, w as f32 / 2.0, sigmas[c], 0.02, 0.8);
                    let n: f32 = (0..12).map(|_| uniform()).sum();
                    data[(y * w + x) * 3 + c] = (v.sqrt() + noise * n).max(0.0).powi(2);
                }
            }
        }
        data
    }

    /// Summed chroma of the middle row's pixels `xs`.
    fn row_chroma(data: &[f32], w: usize, h: usize, xs: std::ops::Range<usize>) -> f32 {
        let y = h / 2;
        xs.map(|x| chroma(&data[(y * w + x) * 3..])).sum()
    }

    #[test]
    fn a_wider_blur_of_one_colour_loses_its_fringe() {
        // Blue blurred three times wider: a blue fringe on the dark side, a
        // yellow one on the bright side; red and blue wider: purple.
        for sigmas in [[1.0, 1.0, 3.0], [2.5, 1.0, 2.5], [1.0, 2.5, 1.0]] {
            let (w, h) = (64, 40);
            let before = edge_image(w, h, sigmas, 0.0);
            let mut after = before.clone();
            apply(&mut after, w, h, 3, 0.0, 1.0);
            let span = w / 2 - 8..w / 2 + 8;
            let (c0, c1) = (
                row_chroma(&before, w, h, span.clone()),
                row_chroma(&after, w, h, span),
            );
            assert!(c1 < 0.25 * c0, "{sigmas:?}: fringe chroma {c0} -> {c1}");
        }
    }

    #[test]
    fn noisy_photos_lose_the_fringe_too() {
        let (w, h) = (64, 40);
        let noise = 0.02;
        let before = edge_image(w, h, [1.0, 1.0, 3.0], noise);
        let mut after = before.clone();
        apply(&mut after, w, h, 3, noise, 1.0);
        // Averaged over the rows, the fringe's colour is mostly gone.
        let mean = |data: &[f32], x: usize| -> [f32; 3] {
            let mut sum = [0.0f32; 3];
            for y in 0..h {
                for c in 0..3 {
                    sum[c] += data[(y * w + x) * 3 + c] / h as f32;
                }
            }
            sum
        };
        let span = w / 2 - 6..w / 2 + 6;
        let fringe = |data: &[f32]| span.clone().map(|x| chroma(&mean(data, x))).sum::<f32>();
        let (c0, c1) = (fringe(&before), fringe(&after));
        assert!(c1 < 0.35 * c0, "fringe chroma {c0} -> {c1}");
    }

    #[test]
    fn a_coloured_stroke_between_the_sides_keeps_its_colour() {
        // White | 3 px red stroke | black, slightly blurred: a third colour
        // whose channels a mix of the sides cannot give in this order.
        let (w, h) = (64, 40);
        let mut data = vec![0.0f32; w * h * 3];
        for y in 0..h {
            for x in 0..w {
                let colour = match x {
                    0..=30 => [0.85, 0.85, 0.85],
                    31..=33 => [0.6, 0.06, 0.04],
                    _ => [0.02, 0.02, 0.02],
                };
                data[(y * w + x) * 3..(y * w + x) * 3 + 3].copy_from_slice(&colour);
            }
        }
        let before = data.clone();
        apply(&mut data, w, h, 3, 0.0, 1.0);
        for x in 31..=33 {
            let i = ((h / 2) * w + x) * 3;
            let (c0, c1) = (chroma(&before[i..]), chroma(&data[i..]));
            assert!(c1 > 0.95 * c0, "x {x}: chroma {c0} -> {c1}");
        }
    }

    #[test]
    fn a_warm_glow_beside_a_light_keeps_its_colour() {
        // A white light with an orange halo fading into dark surroundings:
        // a large deviation from the sides' mix outside the purple and
        // green families.
        let (w, h) = (64, 40);
        let mut data = vec![0.0f32; w * h * 3];
        for y in 0..h {
            for x in 0..w {
                let core = step(x as f32, 34.0, 1.0, 0.0, 0.9);
                let halo = step(x as f32, 30.0, 3.0, 0.0, 0.25);
                let px = [
                    0.02 + core + halo,
                    0.02 + core + 0.45 * halo,
                    0.02 + core + 0.1 * halo,
                ];
                data[(y * w + x) * 3..(y * w + x) * 3 + 3].copy_from_slice(&px);
            }
        }
        let before = data.clone();
        apply(&mut data, w, h, 3, 0.0, 1.0);
        let span = 22..36;
        let (c0, c1) = (
            row_chroma(&before, w, h, span.clone()),
            row_chroma(&data, w, h, span),
        );
        assert!(c1 > 0.9 * c0, "glow chroma {c0} -> {c1}");
    }

    #[test]
    fn a_small_warm_patch_amid_a_corrected_fringe_keeps_its_colour() {
        // A blue fringe on the dark side of a bright edge with a 2 × 2
        // orange patch in it: the neighbours move, the patch (a hue outside
        // the families, far above the noise) does not follow them.
        let (w, h) = (64, 40);
        let mut data = edge_image(w, h, [1.0, 1.0, 3.0], 0.0);
        let patch = [(28, 19), (29, 19), (28, 20), (29, 20)];
        for &(x, y) in &patch {
            data[(y * w + x) * 3..(y * w + x) * 3 + 3].copy_from_slice(&[0.09, 0.035, 0.01]);
        }
        let before = data.clone();
        apply(&mut data, w, h, 3, 0.0, 1.0);
        let fringe = |d: &[f32]| row_chroma(d, w, h, w / 2 - 8..w / 2 + 8);
        assert!(
            fringe(&data) < fringe(&before),
            "the fringe around it moves"
        );
        for &(x, y) in &patch {
            let i = (y * w + x) * 3;
            assert_eq!(&data[i..i + 3], &before[i..i + 3], "patch pixel {x},{y}");
        }
    }

    #[test]
    fn flat_and_soft_areas_are_untouched() {
        let (w, h) = (48, 48);
        let noisy = edge_image(w, h, [40.0, 40.0, 40.0], 0.01);
        let mut after = noisy.clone();
        apply(&mut after, w, h, 3, 0.01, 1.0);
        assert_eq!(after, noisy);
    }

    #[test]
    fn tiles_give_the_whole_image_result() {
        let (w, h) = (97, 83);
        let mut data = edge_image(w, h, [1.0, 1.5, 3.0], 0.01);
        // Diagonal edges and a few spots across tile borders.
        for y in 0..h {
            for x in 0..w {
                if (x + 2 * y) % 37 < 4 || (x * 7 + y * 3) % 53 == 0 {
                    data[(y * w + x) * 3..(y * w + x) * 3 + 3].copy_from_slice(&[0.9, 0.5, 0.95]);
                }
            }
        }
        let mut whole = data.clone();
        apply_in_tiles(&mut whole, (w, h, 3), 0.01, 1.0, (h, w));
        assert_ne!(whole, data, "the stage changes the test image");
        for tiles in [(5, 7), (16, 33), (64, 256)] {
            let mut tiled = data.clone();
            apply_in_tiles(&mut tiled, (w, h, 3), 0.01, 1.0, tiles);
            assert_eq!(tiled, whole, "tiles {tiles:?}");
        }
    }

    #[test]
    fn running_extremes_match_a_plain_scan() {
        let values: Vec<f32> = (0..53)
            .map(|i| ((i * 37) % 23) as f32 - (i % 5) as f32)
            .collect();
        let mut scratch = Vec::new();
        for r in [0usize, 1, 3, 9, 60] {
            let mut lo = vec![0.0f32; values.len()];
            let mut hi = vec![0.0f32; values.len()];
            running(&values, r, f32::min, f32::INFINITY, &mut lo, &mut scratch);
            running(
                &values,
                r,
                f32::max,
                f32::NEG_INFINITY,
                &mut hi,
                &mut scratch,
            );
            for x in 0..values.len() {
                let window = &values[x.saturating_sub(r)..(x + r + 1).min(values.len())];
                assert_eq!(lo[x], window.iter().copied().fold(f32::INFINITY, f32::min));
                assert_eq!(
                    hi[x],
                    window.iter().copied().fold(f32::NEG_INFINITY, f32::max)
                );
            }
        }
    }
}
