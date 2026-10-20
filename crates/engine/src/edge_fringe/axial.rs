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

#[cfg(test)]
#[path = "axial_reference.rs"]
mod reference;

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
/// Contrast ratios safely beyond those stops, where `log2` cannot round
/// across them: full and no change without the logarithm.
const CONTRAST_FULL: f32 = 5.657_42; // 2^2.5 · (1 + 1e-4)
const CONTRAST_NONE: f32 = 2.828_144; // 2^1.5 · (1 − 1e-4)
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
    let off = wrap(wrap(hue - 0.5 * (lo + hi)) + 180.0) - 180.0;
    1.0 - smooth(off.abs(), half, half + HUE_SHOULDER)
}

/// `v.rem_euclid(360.0)` without the library call where it is plain
/// arithmetic: `fmod` returns `v` itself strictly inside ±360 (at -360 it
/// returns -0) and, from 360 to 720, `v - 360`, which the subtraction gives
/// exactly there (Sterbenz).
fn wrap(v: f32) -> f32 {
    if (0.0..360.0).contains(&v) {
        v
    } else if v > -360.0 && v < 0.0 {
        v + 360.0
    } else if (360.0..720.0).contains(&v) {
        v - 360.0
    } else {
        v.rem_euclid(360.0)
    }
}

/// The hue family of a deviation (`da`, `db` in Oklab) without its
/// angle where that cannot matter: deviations well inside an arc where
/// [`in_hue`] is constant (full membership of a family, or none of
/// either). The arcs' edges sit [`ARC_MARGIN`] inside the true ones, far
/// beyond the rounding of `atan2` and the degree arithmetic (about 1e-5°),
/// so the shortcut gives what the angle would.
struct HueArcs {
    /// Arcs (unit vectors at their counter-clockwise start and end) where
    /// a family holds fully, and where neither holds at all.
    full: [[(f32, f32); 2]; 2],
    none: [[(f32, f32); 2]; 2],
}

const ARC_MARGIN: f32 = 0.01;

impl HueArcs {
    fn new() -> Self {
        let arc = |from: f32, to: f32| {
            [from + ARC_MARGIN, to - ARC_MARGIN].map(|d: f32| {
                let r = d.to_radians();
                (r.cos(), r.sin())
            })
        };
        let s = HUE_SHOULDER;
        Self {
            full: [arc(PURPLE.0, PURPLE.1), arc(GREEN.0, GREEN.1)],
            none: [
                arc(GREEN.1 + s, PURPLE.0 - s),
                arc(PURPLE.1 + s - 360.0, GREEN.0 - s),
            ],
        }
    }

    /// Whether (`x`, `y`) lies strictly counter-clockwise of an arc's start
    /// and clockwise of its end (each arc spans less than half a turn).
    fn holds(arcs: &[[(f32, f32); 2]; 2], x: f32, y: f32) -> bool {
        arcs.iter()
            .any(|[(c0, s0), (c1, s1)]| c0 * y - s0 * x > 0.0 && x * s1 - y * c1 > 0.0)
    }
}

static HUE_ARCS: std::sync::LazyLock<HueArcs> = std::sync::LazyLock::new(HueArcs::new);

/// A Gaussian-filtered plane (square-root luminance or a colour) over a
/// rectangle of the image, valid wherever the pixels that read it read;
/// sampled in image coordinates.
#[derive(Clone, Copy)]
struct Plane<'a> {
    values: &'a [f32],
    x0: usize,
    y0: usize,
    w: usize,
}

impl Plane<'_> {
    /// Bilinear read at the offset (`ox`, `oy`) from the pixel (`x`, `y`),
    /// clamped to the image (`width × height`), which the plane covers
    /// there. The interpolation weights come from the offset alone, so a
    /// region of the image reads the same values as the whole image (an
    /// absolute position would round differently at another origin).
    fn read(
        &self,
        (width, height): (usize, usize),
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
        let at = |px: usize, py: usize| self.values[(py - self.y0) * self.w + px - self.x0];
        let top = at(xa, ya) * (1.0 - fx) + at(xb, ya) * fx;
        let bottom = at(xa, yb) * (1.0 - fx) + at(xb, yb) * fx;
        top * (1.0 - fy) + bottom * fy
    }

    /// [`Plane::read`] where the four pixels read lie inside the image:
    /// the same arithmetic without the clamps.
    #[inline(always)]
    fn read_inside(&self, (x, y): (usize, usize), ox: f32, oy: f32) -> f32 {
        let (wx, wy) = (ox.floor(), oy.floor());
        let (fx, fy) = (ox - wx, oy - wy);
        let xa = (x as isize + wx as isize) as usize;
        let ya = (y as isize + wy as isize) as usize;
        let i = (ya - self.y0) * self.w + xa - self.x0;
        let v = self.values;
        let top = v[i] * (1.0 - fx) + v[i + 1] * fx;
        let bottom = v[i + self.w] * (1.0 - fx) + v[i + self.w + 1] * fx;
        top * (1.0 - fy) + bottom * fy
    }

    /// [`Plane::read_inside`] at the offsets `k[i] · (dx, dy)`, eight at a
    /// time (the same arithmetic per lane).
    #[inline(always)]
    fn read_inside_8(&self, (x, y): (usize, usize), k: &[f32; 8], dx: f32, dy: f32) -> [f32; 8] {
        let (mut fx, mut fy, mut at) = ([0.0f32; 8], [0.0f32; 8], [0usize; 8]);
        for i in 0..8 {
            let (ox, oy) = (k[i] * dx, k[i] * dy);
            let (wx, wy) = (ox.floor(), oy.floor());
            fx[i] = ox - wx;
            fy[i] = oy - wy;
            let xa = (x as isize + wx as isize) as usize;
            let ya = (y as isize + wy as isize) as usize;
            at[i] = (ya - self.y0) * self.w + xa - self.x0;
        }
        let v = self.values;
        let mut corners = [[0.0f32; 8]; 4];
        for (i, &j) in at.iter().enumerate() {
            corners[0][i] = v[j];
            corners[1][i] = v[j + 1];
            corners[2][i] = v[j + self.w];
            corners[3][i] = v[j + self.w + 1];
        }
        let mut out = [0.0f32; 8];
        for i in 0..8 {
            let top = corners[0][i] * (1.0 - fx[i]) + corners[1][i] * fx[i];
            let bottom = corners[2][i] * (1.0 - fx[i]) + corners[3][i] * fx[i];
            out[i] = top * (1.0 - fy[i]) + bottom * fy[i];
        }
        out
    }
}

/// The planes a pixel's judging reads: the luminance profile of the
/// two-step test and the side colours.
struct Planes<'a> {
    profile: Plane<'a>,
    colour: [Plane<'a>; 3],
}

/// Where a pixel's side search found the edge's sides.
#[derive(Clone, Copy)]
struct Sides {
    x: usize,
    y: usize,
    /// Whether every read lies inside the image (no clamping).
    inside: bool,
    dx: f32,
    dy: f32,
    to_bright: f32,
    to_dark: f32,
    edge: f32,
}

/// One thread's buffers, reused from tile to tile.
#[derive(Default)]
struct Scratch {
    padded: [Vec<f32>; 4],
    direction: Vec<f32>,
    profile: Vec<f32>,
    colour: [Vec<f32>; 3],
    across: Vec<f32>,
    sides: Vec<Sides>,
    moves: Vec<Option<Move>>,
}

/// The square-root luminance and the three channels of the rectangle
/// `x0.., y0..` of `w × h` and `pad` pixels around it, read with the
/// image's edges (`width × height`) repeated, as the whole image repeats
/// them, into `out`.
fn padded(
    data: &[f32],
    stride: usize,
    (width, height): (usize, usize),
    (x0, y0, w, h): (usize, usize, usize, usize),
    pad: usize,
    out: &mut [Vec<f32>; 4],
) {
    match stride {
        3 => padded_with::<3>(data, (width, height), (x0, y0, w, h), pad, out),
        4 => padded_with::<4>(data, (width, height), (x0, y0, w, h), pad, out),
        _ => padded_with_stride(data, stride, (width, height), (x0, y0, w, h), pad, out),
    }
}

/// [`padded`] for pixels of `S` floats: rows whose columns need no
/// repeated edge are read as runs.
fn padded_with<const S: usize>(
    data: &[f32],
    (width, height): (usize, usize),
    (x0, y0, w, h): (usize, usize, usize, usize),
    pad: usize,
    out: &mut [Vec<f32>; 4],
) {
    if x0 < pad || x0 + w + pad > width {
        return padded_with_stride(data, S, (width, height), (x0, y0, w, h), pad, out);
    }
    let pw = w + 2 * pad;
    let len = pw * (h + 2 * pad);
    for plane in out.iter_mut() {
        plane.clear();
        plane.resize(len, 0.0);
    }
    let [roots, red, green, blue] = out;
    for j in 0..h + 2 * pad {
        let y = (y0 as isize + j as isize - pad as isize).clamp(0, height as isize - 1) as usize;
        let row = &data[(y * width + x0 - pad) * S..(y * width + x0 - pad + pw) * S];
        let (pixels, _) = row.as_chunks::<S>();
        let span = j * pw..(j + 1) * pw;
        for ((((p, l), r), g), b) in pixels
            .iter()
            .zip(&mut roots[span.clone()])
            .zip(&mut red[span.clone()])
            .zip(&mut green[span.clone()])
            .zip(&mut blue[span])
        {
            *l = lum([p[0], p[1], p[2]]).max(0.0).sqrt();
            *r = p[0];
            *g = p[1];
            *b = p[2];
        }
    }
}

fn padded_with_stride(
    data: &[f32],
    stride: usize,
    (width, height): (usize, usize),
    (x0, y0, w, h): (usize, usize, usize, usize),
    pad: usize,
    out: &mut [Vec<f32>; 4],
) {
    let pw = w + 2 * pad;
    let len = pw * (h + 2 * pad);
    for plane in out.iter_mut() {
        plane.clear();
        plane.resize(len, 0.0);
    }
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
}

/// The Gaussian `kernel` of the `w × h` rectangle at (`ox`, `oy`) of a
/// plane whose values with `pad` pixels around it are `source` (rows of
/// `sw`, `kernel.radius <= pad`), into `out` (`across` is scratch). Each
/// output adds its taps in kernel order, so any rectangle gets the values
/// of the whole plane.
fn gaussian(
    source: &[f32],
    (sw, pad): (usize, usize),
    (ox, oy, w, h): (usize, usize, usize, usize),
    kernel: Kernel,
    out: &mut Vec<f32>,
    across: &mut Vec<f32>,
) {
    let weights = kernel.weights();
    let r = kernel.radius;
    // Rows oy + pad - r .. oy + pad + h + r of the source, filtered along x.
    let rows = h + 2 * r;
    across.clear();
    across.resize(rows * w, 0.0);
    for (e, out) in across.chunks_exact_mut(w).enumerate() {
        let row = &source[(oy + pad - r + e) * sw + ox + pad - r..][..w + 2 * r];
        taps_into(out, |k| &row[k..k + w], &weights);
    }
    out.clear();
    out.resize(w * h, 0.0);
    for (y, row) in out.chunks_exact_mut(w).enumerate() {
        taps_into(row, |k| &across[(y + k) * w..(y + k + 1) * w], &weights);
    }
}

/// `out[i] = 0 + weights[0] · tap(0)[i] + weights[1] · tap(1)[i] + …`,
/// added in that order, eight outputs at a time in registers.
#[inline(always)]
fn taps_into<'a>(out: &mut [f32], tap: impl Fn(usize) -> &'a [f32], weights: &[f32]) {
    let n = out.len();
    let whole = n / 8 * 8;
    for b in (0..whole).step_by(8) {
        let mut acc = [0.0f32; 8];
        for (k, weight) in weights.iter().enumerate() {
            let v: &[f32; 8] = tap(k)[b..b + 8].try_into().unwrap();
            for i in 0..8 {
                acc[i] += weight * v[i];
            }
        }
        out[b..b + 8].copy_from_slice(&acc);
    }
    for i in whole..n {
        let mut acc = 0.0f32;
        for (k, weight) in weights.iter().enumerate() {
            acc += weight * tap(k)[i];
        }
        out[i] = acc;
    }
}

/// Smallest and largest of `values` over every window of `len`
/// consecutive values (`lo[i]`, `hi[i]` over `values[i..i + len]`,
/// `values.len() = lo.len() + len - 1`), by doubling runs (`runs` is
/// scratch), so whole rows vectorise. `min` and `max` give the same
/// extreme in any order, and skip NaN.
fn window_runs(
    values: &[f32],
    len: usize,
    (lo, hi): (&mut [f32], &mut [f32]),
    runs: &mut [Vec<f32>; 4],
) {
    let [lo_a, hi_a, lo_b, hi_b] = runs;
    lo_a.clear();
    lo_a.extend_from_slice(values);
    hi_a.clear();
    hi_a.extend_from_slice(values);
    let mut span = 1;
    while 2 * span <= len {
        let n = lo_a.len() - span;
        lo_b.clear();
        lo_b.extend(lo_a[..n].iter().zip(&lo_a[span..]).map(|(a, b)| a.min(*b)));
        hi_b.clear();
        hi_b.extend(hi_a[..n].iter().zip(&hi_a[span..]).map(|(a, b)| a.max(*b)));
        std::mem::swap(lo_a, lo_b);
        std::mem::swap(hi_a, hi_b);
        span *= 2;
    }
    let n = lo.len();
    let far = len - span;
    for ((o, a), b) in lo.iter_mut().zip(&lo_a[..n]).zip(&lo_a[far..far + n]) {
        *o = a.min(*b);
    }
    for ((o, a), b) in hi.iter_mut().zip(&hi_a[..n]).zip(&hi_a[far..far + n]) {
        *o = a.max(*b);
    }
}

/// Whether the luminance varies by at least `low` in the square of radius
/// `r` around each pixel of the region (`rx`, `ry`, `rw`, `rh`) of `plane`
/// (`w × h`), the square clipped to the plane.
fn near_edges(
    plane: &[f32],
    (w, h): (usize, usize),
    r: usize,
    (rx, ry, rw, rh): (usize, usize, usize, usize),
    low: f32,
) -> Vec<bool> {
    let len = 2 * r + 1;
    // The columns and rows the region's squares read, and NaN for the
    // plane's clipped edges (`min` and `max` return the other value).
    let (x0, x1) = (rx.saturating_sub(r), (rx + rw + r).min(w));
    let (y0, y1) = (ry.saturating_sub(r), (ry + rh + r).min(h));
    let left = r - (rx - x0);
    let top = r - (ry - y0);
    let rows = rh + 2 * r;
    let mut lo = vec![f32::NAN; rows * rw];
    let mut hi = vec![f32::NAN; rows * rw];
    let mut runs: [Vec<f32>; 4] = Default::default();
    // Along each row, then along the columns (whole rows at a time).
    let mut padded = vec![f32::NAN; rw + 2 * r];
    for (j, y) in (y0..y1).enumerate() {
        padded[left..left + x1 - x0].copy_from_slice(&plane[y * w + x0..y * w + x1]);
        let at = (top + j) * rw..(top + j + 1) * rw;
        let (lo_row, hi_row) = (&mut lo[at.clone()], &mut hi[at]);
        window_runs(&padded, len, (lo_row, hi_row), &mut runs);
    }
    let mut span = 1;
    let mut count = rows;
    while 2 * span <= len {
        count -= span;
        for i in 0..count {
            let (a, b) = lo.split_at_mut((i + span) * rw);
            for (a, &b) in a[i * rw..(i + 1) * rw].iter_mut().zip(&b[..rw]) {
                *a = a.min(b);
            }
            let (a, b) = hi.split_at_mut((i + span) * rw);
            for (a, &b) in a[i * rw..(i + 1) * rw].iter_mut().zip(&b[..rw]) {
                *a = a.max(b);
            }
        }
        span *= 2;
    }
    let far = (len - span) * rw;
    (0..rh * rw)
        .map(|i| hi[i].max(hi[i + far]) - lo[i].min(lo[i + far]) >= low)
        .collect()
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
        .map_init(Scratch::default, |scratch, tile| {
            let (band, column) = (tile / across, tile % across);
            let rows = band * tile_rows..((band + 1) * tile_rows).min(h);
            let columns = column * tile_cols..((column + 1) * tile_cols).min(w);
            tile_changes(
                scratch,
                source,
                (w, h, stride),
                noise,
                &geometry,
                rows,
                columns,
            )
        })
        .collect();
    // Each band of tiles writes its own rows.
    data.par_chunks_mut(tile_rows * w * stride)
        .zip(changes.par_chunks(across))
        .enumerate()
        .for_each(|(band, (rows, tiles))| {
            let first = band * tile_rows * w;
            for &(i, px) in tiles.iter().flatten() {
                let i = i - first;
                rows[i * stride..i * stride + 3].copy_from_slice(&px);
            }
        });
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
    scratch: &mut Scratch,
    data: &[f32],
    (width, height, stride): (usize, usize, usize),
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
    // Planes around the tile as far as its ring's pixels read, and the
    // image around them as far as their Gaussians read.
    let far = geometry.search + geometry.push.ceil() as usize + 2;
    let px0 = columns.start.saturating_sub(far);
    let py0 = rows.start.saturating_sub(far);
    let pw = (columns.end + far).min(width) - px0;
    let ph = (rows.end + far).min(height) - py0;
    let pixel = |x: usize, y: usize| &data[(y * width + x) * stride..(y * width + x) * stride + 3];
    let pad = geometry
        .direction
        .radius
        .max(geometry.side.radius)
        .max(geometry.profile.radius);
    let Scratch {
        padded: sources,
        direction,
        profile,
        colour,
        across,
        sides,
        moves,
    } = scratch;
    padded(
        data,
        stride,
        (width, height),
        (px0, py0, pw, ph),
        pad,
        sources,
    );
    let [roots, red, green, blue] = &*sources;
    let source = (pw + 2 * pad, pad);
    gaussian(
        roots,
        source,
        (0, 0, pw, ph),
        geometry.direction,
        direction,
        across,
    );
    let direction = Plane {
        values: direction,
        x0: px0,
        y0: py0,
        w: pw,
    };
    // Every luminance a pixel's side search reads lies within `search + 1`
    // of it; where those vary less than the edge test's start, the pixel
    // cannot change and is skipped (exactly: a necessary condition).
    let low = STEP_LOW.max(STEP_NOISE * noise);
    let rw = ring_x1 - ring_x0;
    let rh = ring_y1 - ring_y0;
    let near = near_edges(
        direction.values,
        (pw, ph),
        geometry.search + 1,
        (ring_x0 - px0, ring_y0 - py0, rw, rh),
        low,
    );
    // The side search first, then the side colours and the profile only
    // around the pixels it found (`reads` around each), judged eight at a
    // time.
    sides.clear();
    let (mut bx0, mut by0, mut bx1, mut by1) = (usize::MAX, usize::MAX, 0, 0);
    for y in ring_y0..ring_y1 {
        for x in ring_x0..ring_x1 {
            if !near[(y - ring_y0) * rw + x - ring_x0] {
                continue;
            }
            if let Some(found) = search(
                direction,
                pixel(x, y),
                (x, y),
                (width, height),
                low,
                geometry,
            ) {
                sides.push(found);
                (bx0, by0) = (bx0.min(x), by0.min(y));
                (bx1, by1) = (bx1.max(x + 1), by1.max(y + 1));
            }
        }
    }
    if sides.is_empty() {
        return Vec::new();
    }
    let reads = far - 1;
    let bx0 = bx0.saturating_sub(reads).max(px0);
    let by0 = by0.saturating_sub(reads).max(py0);
    let bw = (bx1 + reads).min(px0 + pw) - bx0;
    let bh = (by1 + reads).min(py0 + ph) - by0;
    let rect = (bx0 - px0, by0 - py0, bw, bh);
    gaussian(roots, source, rect, geometry.profile, profile, across);
    for (out, plane) in colour.iter_mut().zip([red, green, blue]) {
        gaussian(plane, source, rect, geometry.side, out, across);
    }
    let around = |values| Plane {
        values,
        x0: bx0,
        y0: by0,
        w: bw,
    };
    let planes = Planes {
        profile: around(profile),
        colour: colour.each_ref().map(|c| around(c)),
    };
    moves.clear();
    moves.resize(rw * rh, None);
    for chunk in sides.chunks(8) {
        // Lanes past the last pixel repeat it (their moves are dropped).
        let found: [Sides; 8] = std::array::from_fn(|i| chunk[i.min(chunk.len() - 1)]);
        let pixels = found.map(|f| {
            let p = pixel(f.x, f.y);
            [p[0], p[1], p[2]]
        });
        let mut out = [None; 8];
        let size = (width, height);
        judge_8(
            &planes,
            &pixels,
            &found,
            chunk.len(),
            size,
            noise,
            geometry,
            &mut out,
        );
        for (f, m) in chunk.iter().zip(out) {
            moves[(f.y - ring_y0) * rw + f.x - ring_x0] = m;
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
            // (Where the cap is the weight, the median cannot raise it.)
            let weight = if own.cap == own.weight {
                own.weight
            } else {
                let mut near = [0.0f32; 9];
                let mut n = 0;
                for ny in y.saturating_sub(1)..(y + 2).min(height) {
                    for nx in x.saturating_sub(1)..(x + 2).min(width) {
                        near[n] =
                            moves[(ny - ring_y0) * rw + nx - ring_x0].map_or(0.0, |m| m.weight);
                        n += 1;
                    }
                }
                let near = &mut near[..n];
                near.sort_by(f32::total_cmp);
                own.weight.max(near[n / 2].min(own.cap))
            };
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

/// The edge's sides along the luminance gradient at the pixel `p` at
/// (`x`, `y`); None away from strong edges (a luminance step below
/// `low`).
#[inline(always)]
fn search(
    direction: Plane,
    p: &[f32],
    (x, y): (usize, usize),
    size: (usize, usize),
    low: f32,
    geometry: &Geometry,
) -> Option<Sides> {
    if p[..3].iter().any(|c| !c.is_finite()) {
        return None;
    }
    let (width, height) = size;
    // Every read lies within `search + push + 2` of the pixel: away from
    // the image's edges the reads need no clamping.
    let margin = geometry.search + geometry.push.ceil() as usize + 2;
    let inside = x >= margin && y >= margin && x + margin < width && y + margin < height;
    let lum_at = |dx: f32, dy: f32| {
        if inside {
            direction.read_inside((x, y), dx, dy)
        } else {
            direction.read(size, (x, y), dx, dy)
        }
    };
    let gx = 0.5 * (lum_at(1.0, 0.0) - lum_at(-1.0, 0.0));
    let gy = 0.5 * (lum_at(0.0, 1.0) - lum_at(0.0, -1.0));
    let g = gx.hypot(gy);
    // (NaN included.)
    if g.is_nan() || g <= 1e-5 {
        return None;
    }
    let (dx, dy) = (gx / g, gy / g);
    // The brightest point along the gradient and the darkest against it.
    let own = lum_at(0.0, 0.0);
    let (mut bright, mut to_bright) = (own, 0.0f32);
    let (mut dark, mut to_dark) = (own, 0.0f32);
    if inside {
        // Steps 1 ..= search (at most 8) in lanes; the rest read the pixel
        // itself and are not looked at.
        let ks: [f32; 8] = std::array::from_fn(|i| {
            if i < geometry.search {
                (i + 1) as f32
            } else {
                0.0
            }
        });
        let ahead = direction.read_inside_8((x, y), &ks, dx, dy);
        let behind = direction.read_inside_8((x, y), &ks.map(|k| -k), dx, dy);
        // (Selects rather than branches: which step wins is a coin toss.)
        for i in 0..geometry.search {
            let ahead_wins = ahead[i] > bright;
            bright = if ahead_wins { ahead[i] } else { bright };
            to_bright = if ahead_wins { ks[i] } else { to_bright };
            let behind_wins = behind[i] < dark;
            dark = if behind_wins { behind[i] } else { dark };
            to_dark = if behind_wins { ks[i] } else { to_dark };
        }
    } else {
        for k in 1..=geometry.search {
            let k = k as f32;
            let ahead = lum_at(k * dx, k * dy);
            if ahead > bright {
                bright = ahead;
                to_bright = k;
            }
            let behind = lum_at(-k * dx, -k * dy);
            if behind < dark {
                dark = behind;
                to_dark = k;
            }
        }
    }
    let edge = smooth(bright - dark, low, low + STEP_SPAN);
    (edge > 0.0).then_some(Sides {
        x,
        y,
        inside,
        dx,
        dy,
        to_bright,
        to_dark,
        edge,
    })
}

/// Eight lanes of `f32`.
type Lanes = [f32; 8];

/// `f` on each lane (written out per lane, so the loop vectorises).
#[inline(always)]
fn lanes(f: impl Fn(usize) -> f32) -> Lanes {
    let mut out = [0.0f32; 8];
    for (i, o) in out.iter_mut().enumerate() {
        *o = f(i);
    }
    out
}

/// Where the first `count` of eight pixels (colours `pixels`) whose side
/// search found the sides `found` move and how far (None where a pixel
/// keeps its colour), into `out`: lane by lane the arithmetic of one
/// pixel, so whole lanes vectorise.
#[allow(clippy::too_many_arguments)]
#[inline(always)]
fn judge_8(
    planes: &Planes,
    pixels: &[[f32; 3]; 8],
    found: &[Sides; 8],
    count: usize,
    size: (usize, usize),
    noise: f32,
    geometry: &Geometry,
    out: &mut [Option<Move>; 8],
) {
    let push = geometry.push;
    // The side colours (gathers, lane by lane).
    let mut lit = [[0.0f32; 8]; 3];
    let mut shade = [[0.0f32; 8]; 3];
    for i in 0..8 {
        let f = &found[i];
        let (x, y) = (f.x, f.y);
        let (kl, ks) = (f.to_bright + push, -(f.to_dark + push));
        for c in 0..3 {
            let plane = planes.colour[c];
            if f.inside {
                lit[c][i] = plane.read_inside((x, y), kl * f.dx, kl * f.dy);
                shade[c][i] = plane.read_inside((x, y), ks * f.dx, ks * f.dy);
            } else {
                lit[c][i] = plane.read(size, (x, y), kl * f.dx, kl * f.dy);
                shade[c][i] = plane.read(size, (x, y), ks * f.dx, ks * f.dy);
            }
        }
    }
    let p: [Lanes; 3] = std::array::from_fn(|c| lanes(|i| pixels[i][c]));
    let lum8 = |v: &[Lanes; 3]| lanes(|i| 0.25 * (v[0][i] + 2.0 * v[1][i] + v[2][i]));
    let (l_lit, l_shade, l_own) = (lum8(&lit), lum8(&shade), lum8(&p));
    // The side mix at the pixel's luminance.
    let t = lanes(|i| ((l_own[i] - l_shade[i]) / (l_lit[i] - l_shade[i])).clamp(0.0, 1.0));
    let mix: [Lanes; 3] =
        std::array::from_fn(|c| lanes(|i| shade[c][i] + t[i] * (lit[c][i] - shade[c][i])));
    let span = lanes(|i| {
        let mut span = 0.0f32;
        for c in 0..3 {
            span = span.max((lit[c][i] - shade[c][i]).abs());
        }
        span.max(1e-6)
    });
    let beyond = lanes(|i| {
        let mut beyond = 0.0f32;
        for c in 0..3 {
            let tolerance = NOISE_TOLERANCE * 2.0 * p[c][i].max(0.0).sqrt() * noise;
            let lo = lit[c][i].min(shade[c][i]) - tolerance;
            let hi = lit[c][i].max(shade[c][i]) + tolerance;
            beyond = beyond.max((p[c][i] - hi).max(lo - p[c][i]).max(0.0) / span[i]);
        }
        beyond
    });
    let between = lanes(|i| 1.0 - smooth(beyond[i], BEYOND_FULL, BEYOND_NONE));
    let ratio = lanes(|i| l_lit[i].max(1e-6) / l_shade[i].max(1e-6));
    let mut contrast = lanes(|i| {
        if ratio[i] >= CONTRAST_FULL {
            1.0
        } else if ratio[i] <= CONTRAST_NONE {
            0.0
        } else {
            f32::NAN
        }
    });
    for i in 0..count {
        if contrast[i].is_nan() {
            contrast[i] = smooth(ratio[i].log2(), STOPS_LOW, STOPS_HIGH);
        }
    }
    // The mix's colour at the pixel's own luminance, never more colour
    // than the pixel had.
    let l_mix = lum8(&mix);
    let mut colour: [Lanes; 3] = std::array::from_fn(|c| lanes(|i| mix[c][i] - l_mix[i]));
    let norm8 = |v: &[Lanes; 3]| {
        lanes(|i| (v[0][i] * v[0][i] + v[1][i] * v[1][i] + v[2][i] * v[2][i]).sqrt())
    };
    let had = norm8(&std::array::from_fn(|c| lanes(|i| p[c][i] - l_own[i])));
    let gets = norm8(&colour);
    for v in &mut colour {
        *v = lanes(|i| {
            if gets[i] > had[i] && gets[i] > 0.0 {
                v[i] * (had[i] / gets[i])
            } else {
                v[i]
            }
        });
    }
    // Signed, as the linear values beyond sRGB the engine keeps.
    let target: [Lanes; 3] = std::array::from_fn(|c| lanes(|i| l_own[i] + colour[c][i]));
    // Which deviation: the hue families of axial aberration, or small.
    let (a, b) = (oklab_8(&p, count), oklab_8(&target, count));
    let (da, db) = (lanes(|i| a[1][i] - b[1][i]), lanes(|i| a[2][i] - b[2][i]));
    let arcs = &*HUE_ARCS;
    let held = |arcs, i: usize| {
        if HueArcs::holds(arcs, da[i], db[i]) {
            1.0
        } else {
            0.0
        }
    };
    let full = lanes(|i| held(&arcs.full, i));
    let none = lanes(|i| held(&arcs.none, i));
    let edge = lanes(|i| found[i].edge);
    for i in 0..count {
        let lane = |v: &[Lanes; 3]| [v[0][i], v[1][i], v[2][i]];
        let (pi, li, si) = (lane(&p), lane(&lit), lane(&shade));
        if (l_lit[i] - l_shade[i]).is_nan() || l_lit[i] - l_shade[i] <= 1e-6 {
            out[i] = None;
            continue;
        }
        // No contrast moves nothing (every other factor is finite with
        // values of this size, so the weights are exactly zero).
        if contrast[i] == 0.0 && pi.iter().chain(&li).chain(&si).all(|v| v.abs() < 1e15) {
            out[i] = None;
            continue;
        }
        let (da, db) = (da[i], db[i]);
        let small = || {
            if noise > 0.0 {
                1.0 - smooth(da.hypot(db), SMALL_FULL * noise, SMALL_NONE * noise)
            } else {
                0.0
            }
        };
        let family = if full[i] == 1.0 {
            1.0
        } else if none[i] == 1.0 {
            0.0f32.max(small())
        } else {
            let hue = wrap(db.atan2(da).to_degrees());
            in_hue(hue, PURPLE).max(in_hue(hue, GREEN)).max(small())
        };
        let weight = edge[i] * between[i] * contrast[i] * family;
        let cap = edge[i] * contrast[i] * family;
        // The two-step test only where it can matter.
        let single = if cap > 0.0 {
            let dip = two_steps(planes.profile, &found[i], size);
            1.0 - smooth(dip, STEPS_FULL, STEPS_NONE)
        } else {
            1.0
        };
        out[i] = Some(Move {
            weight: weight * single,
            cap: cap * single,
            target: lane(&target),
        });
    }
}

/// [`oklab`] in eight lanes (the cube roots lane by lane).
#[inline(always)]
fn oklab_8(c: &[Lanes; 3], count: usize) -> [Lanes; 3] {
    let l = lanes(|i| 0.412_221_47 * c[0][i] + 0.536_332_55 * c[1][i] + 0.051_445_995 * c[2][i]);
    let m = lanes(|i| 0.211_903_5 * c[0][i] + 0.680_699_5 * c[1][i] + 0.107_396_96 * c[2][i]);
    let s = lanes(|i| 0.088_302_46 * c[0][i] + 0.281_718_85 * c[1][i] + 0.629_978_7 * c[2][i]);
    let (mut l, mut m, mut s) = (
        lanes(|i| l[i].max(0.0)),
        lanes(|i| m[i].max(0.0)),
        lanes(|i| s[i].max(0.0)),
    );
    for i in 0..count {
        (l[i], m[i], s[i]) = (l[i].cbrt(), m[i].cbrt(), s[i].cbrt());
    }
    [
        lanes(|i| 0.210_454_26 * l[i] + 0.793_617_8 * m[i] - 0.004_072_047 * s[i]),
        lanes(|i| 1.977_998_5 * l[i] - 2.428_592_2 * m[i] + 0.450_593_7 * s[i]),
        lanes(|i| 0.025_904_037 * l[i] + 0.782_771_77 * m[i] - 0.808_675_77 * s[i]),
    ]
}

/// The dip between two luminance steps of `profile` on the path from the
/// dark side to the bright side `found`, relative to the steeper step: 0
/// for a single blurred step.
fn two_steps(profile: Plane, found: &Sides, size: (usize, usize)) -> f32 {
    let &Sides {
        x,
        y,
        inside,
        dx,
        dy,
        to_bright,
        to_dark,
        ..
    } = found;
    let first = -(to_dark as i32) - 1;
    let last = to_bright as i32 + 1;
    let mut values = [0.0f32; 64];
    let mut n = 0;
    for k in first..=last {
        let k = k as f32;
        values[n] = if inside {
            profile.read_inside((x, y), k * dx, k * dy)
        } else {
            profile.read(size, (x, y), k * dx, k * dy)
        };
        n += 1;
    }
    let steps = &values[..n];
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
    fn a_wide_gamut_edge_of_one_colour_keeps_its_signed_channels() {
        // A brightness step of one colour beyond sRGB (negative red), the
        // same blur in every channel: nothing to correct, and in particular
        // no channel clipped at zero (which would change luminance).
        let (w, h) = (64, 40);
        let mut data = vec![0.0f32; w * h * 3];
        for y in 0..h {
            for x in 0..w {
                let t = step(x as f32, 32.0, 1.5, 0.02, 1.0);
                data[(y * w + x) * 3..(y * w + x) * 3 + 3].copy_from_slice(&[-0.5 * t, t, t]);
            }
        }
        let before = data.clone();
        apply(&mut data, w, h, 3, 0.0, 1.0);
        for (i, (a, b)) in before
            .as_chunks::<3>()
            .0
            .iter()
            .zip(data.as_chunks::<3>().0)
            .enumerate()
        {
            let (la, lb) = (lum([a[0], a[1], a[2]]), lum([b[0], b[1], b[2]]));
            assert!(
                (la - lb).abs() <= 1e-5 * la.abs().max(1e-3),
                "pixel {i}: luminance {la} -> {lb}"
            );
            for c in 0..3 {
                assert!((a[c] - b[c]).abs() < 1e-3, "pixel {i}: {a:?} -> {b:?}");
            }
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
    fn edge_gate_matches_a_plain_scan() {
        let (w, h) = (23, 17);
        let mut plane: Vec<f32> = (0..w * h)
            .map(|i| ((i * 37) % 23) as f32 * 0.05 - (i % 5) as f32 * 0.03)
            .collect();
        plane[40] = f32::NAN;
        plane[100..105].fill(f32::NAN);
        plane[41] = f32::INFINITY;
        for r in [1usize, 3, 4, 9, 30] {
            for region in [(0, 0, w, h), (3, 2, 9, 11), (20, 15, 3, 2), (5, 0, 1, 1)] {
                let low = 0.4;
                let near = near_edges(&plane, (w, h), r, region, low);
                let (rx, ry, rw, rh) = region;
                for y in ry..ry + rh {
                    for x in rx..rx + rw {
                        let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
                        for yy in y.saturating_sub(r)..(y + r + 1).min(h) {
                            for xx in x.saturating_sub(r)..(x + r + 1).min(w) {
                                lo = lo.min(plane[yy * w + xx]);
                                hi = hi.max(plane[yy * w + xx]);
                            }
                        }
                        assert_eq!(
                            near[(y - ry) * rw + x - rx],
                            hi - lo >= low,
                            "r {r} region {region:?} at {x},{y}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn the_stage_matches_the_reference_bit_for_bit() {
        // Dense diagonal edges on the left, a flat noisy right half with a
        // few spots (tiles of both kinds), and pixels at the extremes.
        let (w, h) = (181, 133);
        let mut data = edge_image(w, h, [1.0, 1.4, 3.0], 0.01);
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) * 3;
                if x < w / 2 && (x + 2 * y) % 23 < 5 {
                    data[i..i + 3].copy_from_slice(&[0.9, 0.5, 0.95]);
                } else if x >= w / 2 {
                    let v = data[i + 1];
                    data[i..i + 3].copy_from_slice(&[v, v, v]);
                    if (x * 7 + y * 3) % 211 == 0 {
                        data[i..i + 3].copy_from_slice(&[0.02, 0.9, 0.7]);
                    }
                }
            }
        }
        let specials = [
            [f32::NAN, 0.5, 0.5],
            [f32::INFINITY, 0.2, 0.1],
            [0.3, f32::NEG_INFINITY, 0.3],
            [1e30, 1e30, 1e30],
            [3e38, 0.0, 3e38],
            [0.0, 0.0, 0.0],
            [-0.0, -0.0, -0.0],
            [-0.4, 0.6, 0.6],
            [40.0, 35.0, 60.0],
        ];
        for (n, special) in specials.iter().enumerate() {
            for (x, y) in [
                (20 + 9 * n, 30),
                (3 * n, 2),
                (w - 1 - n, h - 1),
                (w / 2 + 11 * n, 70),
            ] {
                data[(y * w + x) * 3..(y * w + x) * 3 + 3].copy_from_slice(special);
            }
        }
        let rgba: Vec<f32> = data
            .chunks_exact(3)
            .flat_map(|p| [p[0], p[1], p[2], 0.25])
            .collect();
        let bits = |v: &[f32]| v.iter().map(|v| v.to_bits()).collect::<Vec<_>>();
        for (image, stride) in [(&data, 3), (&rgba, 4)] {
            for (noise, scale) in [(0.01, 1.0), (0.0, 1.0), (0.004, 0.5), (0.02, 0.37)] {
                let mut want = image.clone();
                reference::apply(&mut want, w, h, stride, noise, scale);
                assert_ne!(bits(&want), bits(image), "the stage changes the test image");
                for tiles in [(TILE_ROWS, TILE_COLS), (16, 24), (h, w), (7, 200)] {
                    let mut got = image.clone();
                    apply_in_tiles(&mut got, (w, h, stride), noise, scale, tiles);
                    assert!(
                        bits(&got) == bits(&want),
                        "stride {stride} noise {noise} scale {scale} tiles {tiles:?}: {} values differ",
                        got.iter()
                            .zip(&want)
                            .filter(|(a, b)| a.to_bits() != b.to_bits())
                            .count()
                    );
                }
            }
        }
    }

    #[test]
    fn contrast_shortcuts_lie_beyond_the_stops() {
        assert!(CONTRAST_FULL.log2() > STOPS_HIGH + 1e-5);
        assert!(CONTRAST_NONE.log2() < STOPS_LOW - 1e-5);
    }

    #[test]
    fn hue_arcs_agree_with_the_angle() {
        // Directions all around, at several lengths: wherever an arc
        // claims a deviation, membership from its angle is what it says.
        let arcs = HueArcs::new();
        for step in 0..360_000 {
            let angle = (step as f32 * 0.001).to_radians();
            for length in [1e-6f32, 0.01, 0.3] {
                let (x, y) = (length * angle.cos(), length * angle.sin());
                let hue = wrap(y.atan2(x).to_degrees());
                let member = in_hue(hue, PURPLE).max(in_hue(hue, GREEN));
                if HueArcs::holds(&arcs.full, x, y) {
                    assert_eq!(member, 1.0, "angle {}", step as f32 * 0.001);
                }
                if HueArcs::holds(&arcs.none, x, y) {
                    assert_eq!(member, 0.0, "angle {}", step as f32 * 0.001);
                }
            }
        }
    }

    #[test]
    fn wrap_is_rem_euclid() {
        for v in [
            -720.5f32,
            -360.0,
            -359.99,
            -1e-30,
            -0.0,
            0.0,
            1e-30,
            180.0,
            359.999_97,
            360.0,
            540.0,
            719.99,
            720.0,
            1e9,
            f32::NAN,
            f32::INFINITY,
        ] {
            assert_eq!(wrap(v).to_bits(), v.rem_euclid(360.0).to_bits(), "{v}");
        }
        let mut v = -800.0f32;
        while v < 800.0 {
            assert_eq!(wrap(v).to_bits(), v.rem_euclid(360.0).to_bits(), "{v}");
            v += 0.013;
        }
    }
}
