//! The luminance stage and the colour-line stage as version 3 first
//! shipped them (PR #189: whole rows per band, one window at a time): the
//! oracles the tiled, vectorised stages must match bit for bit
//! (`the_stages_match_the_reference_bit_for_bit`).

use super::{EDGE_HIGH, EDGE_LOW, EPS, enc, near_some};
use super::{LINE_EPS, MIX_FULL, MIX_NONE, PAIRS, TWO_COLOUR_FULL, TWO_COLOUR_NONE};

/// `out[x]` = sum of `row` over `x - r ..= x + r` (edges repeated), using
/// `pad` (at least `row.len() + 2r` long) as scratch.
fn box_row(row: &[f32], r: usize, pad: &mut [f32], out: &mut [f32]) {
    let w = row.len();
    let pad = &mut pad[..w + 2 * r];
    pad[..r].fill(row[0]);
    pad[r..r + w].copy_from_slice(row);
    pad[r + w..].fill(row[w - 1]);
    out.copy_from_slice(&pad[..w]);
    for dx in 1..=2 * r {
        for (o, v) in out.iter_mut().zip(&pad[dx..dx + w]) {
            *o += v;
        }
    }
}

/// `out` = sum of the `2r + 1` rows of `rows` (each `w` long) starting at
/// row `first`.
fn box_column(rows: &[f32], w: usize, first: usize, r: usize, out: &mut [f32]) {
    out.copy_from_slice(&rows[first * w..(first + 1) * w]);
    for k in first + 1..=first + 2 * r {
        for (o, v) in out.iter_mut().zip(&rows[k * w..(k + 1) * w]) {
            *o += v;
        }
    }
}
pub(super) fn suppress_band(
    data: &[f32],
    w: usize,
    h: usize,
    stride: usize,
    r: usize,
    y0: usize,
    y1: usize,
) -> Vec<(usize, f32, f32)> {
    let row_of = |y: isize| y.clamp(0, h as isize - 1) as usize;
    let inv_area = 1.0 / ((2 * r + 1) * (2 * r + 1)) as f32;
    let mut pad = vec![0.0f32; w + 2 * r];
    // Rows y0 - 2r .. y1 + 2r: luminance and colour differences, and the
    // horizontal sums of their moments Y, u, v, YY, Yu, Yv.
    let ext0 = y0 as isize - 2 * r as isize;
    let ext_rows = (y1 - y0) + 4 * r;
    let (mut lum, mut du, mut dv) = (
        vec![0.0f32; ext_rows * w],
        vec![0.0f32; ext_rows * w],
        vec![0.0f32; ext_rows * w],
    );
    for e in 0..ext_rows {
        let y = row_of(ext0 + e as isize);
        let src = &data[y * w * stride..(y + 1) * w * stride];
        let span = e * w..(e + 1) * w;
        for (((px, l), u), v) in src
            .chunks_exact(stride)
            .zip(&mut lum[span.clone()])
            .zip(&mut du[span.clone()])
            .zip(&mut dv[span])
        {
            let g = enc(px[1]);
            *l = enc(0.25 * (px[0] + 2.0 * px[1] + px[2]));
            *u = enc(px[0]) - g;
            *v = enc(px[2]) - g;
        }
    }
    let mut hsum: [Vec<f32>; 6] = std::array::from_fn(|_| vec![0.0f32; ext_rows * w]);
    let mut moment = vec![0.0f32; w];
    for e in 0..ext_rows {
        let span = e * w..(e + 1) * w;
        let (l, u, v) = (&lum[span.clone()], &du[span.clone()], &dv[span.clone()]);
        for (k, sum) in hsum.iter_mut().enumerate() {
            match k {
                0 => moment.copy_from_slice(l),
                1 => moment.copy_from_slice(u),
                2 => moment.copy_from_slice(v),
                _ => {
                    let other = [l, u, v][k - 3];
                    for ((m, a), b) in moment.iter_mut().zip(l).zip(other) {
                        *m = a * b;
                    }
                }
            }
            box_row(&moment, r, &mut pad, &mut sum[span.clone()]);
        }
    }
    // Rows y0 - r .. y1 + r: the window's model (slopes and offsets of u and
    // v on Y) and its edge weight, then summed along rows. Mid row m is ext
    // row m + r; its window spans ext rows m ..= m + 2r.
    let mid_rows = (y1 - y0) + 2 * r;
    let mut csum: [Vec<f32>; 5] = std::array::from_fn(|_| vec![0.0f32; mid_rows * w]);
    let mut vs: [Vec<f32>; 6] = std::array::from_fn(|_| vec![0.0f32; w]);
    let mut coef: [Vec<f32>; 5] = std::array::from_fn(|_| vec![0.0f32; w]);
    for m in 0..mid_rows {
        for (k, out) in vs.iter_mut().enumerate() {
            box_column(&hsum[k], w, m, r, out);
        }
        for x in 0..w {
            let my = vs[0][x] * inv_area;
            let mu = vs[1][x] * inv_area;
            let mv = vs[2][x] * inv_area;
            let var = (vs[3][x] * inv_area - my * my).max(0.0);
            let au = (vs[4][x] * inv_area - my * mu) / (var + EPS);
            let av = (vs[5][x] * inv_area - my * mv) / (var + EPS);
            coef[0][x] = au;
            coef[1][x] = mu - au * my;
            coef[2][x] = av;
            coef[3][x] = mv - av * my;
            coef[4][x] = ((var.sqrt() - EDGE_LOW) / (EDGE_HIGH - EDGE_LOW)).clamp(0.0, 1.0);
        }
        let span = m * w..(m + 1) * w;
        for (k, sum) in csum.iter_mut().enumerate() {
            box_row(&coef[k], r, &mut pad, &mut sum[span.clone()]);
        }
    }
    let mut changes = Vec::new();
    let mut model: [Vec<f32>; 5] = std::array::from_fn(|_| vec![0.0f32; w]);
    for y in y0..y1 {
        // Output row y is mid row y - y0 + r (its window: mid rows y - y0 ..=
        // y - y0 + 2r) and ext row y - y0 + 2r.
        let m0 = y - y0;
        for (k, out) in model.iter_mut().enumerate() {
            box_column(&csum[k], w, m0, r, out);
        }
        let e = y - y0 + 2 * r;
        for x in 0..w {
            // Spread over the window, so a fringe a pixel or two beside the
            // edge's centre is covered fully.
            let weight = (2.0 * model[4][x] * inv_area).min(1.0);
            if weight <= 0.0 {
                continue;
            }
            let i = y * w + x;
            let px = &data[i * stride..][..3];
            if px.iter().any(|&c| c < 0.0 || !c.is_finite()) {
                continue;
            }
            let (l, u0, v0) = (lum[e * w + x], du[e * w + x], dv[e * w + x]);
            let mut u1 = (model[0][x] * l + model[1][x]) * inv_area;
            let mut v1 = (model[2][x] * l + model[3][x]) * inv_area;
            let (c0, c1) = (u0.hypot(v0), u1.hypot(v1));
            if c1 > c0 {
                // Never more colour than the pixel had.
                let k = if c1 > 0.0 { c0 / c1 } else { 0.0 };
                u1 *= k;
                v1 *= k;
            }
            let g = enc(px[1]);
            let red = (g + u0 + weight * (u1 - u0)).max(0.0);
            let blue = (g + v0 + weight * (v1 - v0)).max(0.0);
            changes.push((i, red * red, blue * blue));
        }
    }
    changes
}

/// A row of windows' means and covariances (the six [`PAIRS`] entries),
/// principal axes and principal variances, one column per window. Row-wise
/// loops over columns, so they vectorise.
struct RowStats {
    mean: [Vec<f32>; 3],
    cov: [Vec<f32>; 6],
    axis: [Vec<f32>; 3],
    var: Vec<f32>,
}

impl RowStats {
    fn new(w: usize) -> Self {
        Self {
            mean: std::array::from_fn(|_| vec![0.0; w]),
            cov: std::array::from_fn(|_| vec![0.0; w]),
            axis: std::array::from_fn(|_| vec![0.0; w]),
            var: vec![0.0; w],
        }
    }

    /// Means and covariances from the box sums of a row of windows (three
    /// channels, then the six [`PAIRS`] products).
    fn of(&mut self, sums: &[Vec<f32>], inv_area: f32) {
        for (mean, sum) in self.mean.iter_mut().zip(sums) {
            for (m, s) in mean.iter_mut().zip(sum) {
                *m = s * inv_area;
            }
        }
        for (k, &(a, b)) in PAIRS.iter().enumerate() {
            let (ma, mb) = (&self.mean[a], &self.mean[b]);
            for (x, c) in self.cov[k].iter_mut().enumerate() {
                let v = sums[3 + k][x] * inv_area - ma[x] * mb[x];
                *c = if a == b { v.max(0.0) } else { v };
            }
        }
    }

    /// The principal axes (unit vectors) and variances of the windows at
    /// `columns` by power iteration from each covariance's largest column
    /// (three steps: the error falls with (λ2/λ1)³, under 0.4 % where a
    /// window passes the two-colour test); axis and variance 0 for a window
    /// of one colour. Other columns are left as they are.
    fn principal(&mut self, columns: &[usize]) {
        let [c00, c01, c02, c11, c12, c22] = &self.cov;
        let [e0, e1, e2] = &mut self.axis;
        for &x in columns {
            let (a, b, c, d, e, f) = (c00[x], c01[x], c02[x], c11[x], c12[x], c22[x]);
            let n0 = a * a + b * b + c * c;
            let n1 = b * b + d * d + e * e;
            let n2 = c * c + e * e + f * f;
            let mut v = if n0 >= n1 && n0 >= n2 {
                [a, b, c]
            } else if n1 >= n2 {
                [b, d, e]
            } else {
                [c, e, f]
            };
            let mul = |v: [f32; 3]| {
                [
                    a * v[0] + b * v[1] + c * v[2],
                    b * v[0] + d * v[1] + e * v[2],
                    c * v[0] + e * v[1] + f * v[2],
                ]
            };
            for _ in 0..3 {
                let inv = 1.0 / (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
                v = mul(v.map(|t| t * inv));
            }
            let n = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
            let flat = !(n > 1e-12 && n.is_finite());
            let v = if flat { [0.0; 3] } else { v.map(|t| t / n) };
            let cv = mul(v);
            e0[x] = v[0];
            e1[x] = v[1];
            e2[x] = v[2];
            self.var[x] = if flat {
                0.0
            } else {
                (v[0] * cv[0] + v[1] * cv[1] + v[2] * cv[2]).max(0.0)
            };
        }
    }

    /// The colour-line stage's weight for each window from these statistics
    /// of its square-root RGB: near a strong edge (as the luminance pass's
    /// test, a neutral edge's principal variance being three times its
    /// luminance variance) and holding two colours (little variance off its
    /// principal line). `columns` is scratch.
    fn weights(&mut self, out: &mut [f32], columns: &mut Vec<usize>) {
        // The principal variance is at most the total and at most the
        // covariance's Frobenius norm: windows failing either test with
        // those bounds need no principal axis.
        let [c00, c01, c02, c11, c12, c22] = &self.cov;
        columns.clear();
        columns.extend((0..out.len()).filter(|&x| {
            let total = c00[x] + c11[x] + c22[x];
            let off = c01[x] * c01[x] + c02[x] * c02[x] + c12[x] * c12[x];
            let frobenius =
                (c00[x] * c00[x] + c11[x] * c11[x] + c22[x] * c22[x] + 2.0 * off).sqrt();
            let bound = total.min(frobenius);
            bound > 3.0 * EDGE_LOW * EDGE_LOW && total - bound < TWO_COLOUR_NONE * bound
        }));
        out.fill(0.0);
        self.principal(columns);
        let [c00, _, _, c11, _, c22] = &self.cov;
        for &x in columns.iter() {
            let out = &mut out[x];
            let var = self.var[x];
            let total = c00[x] + c11[x] + c22[x];
            let off_line = ((total - var) / var.max(1e-20)).max(0.0);
            let edge = (((var / 3.0).sqrt() - EDGE_LOW) / (EDGE_HIGH - EDGE_LOW)).clamp(0.0, 1.0);
            let two = ((TWO_COLOUR_NONE - off_line) / (TWO_COLOUR_NONE - TWO_COLOUR_FULL))
                .clamp(0.0, 1.0);
            *out = if var > 0.0 { edge * two } else { 0.0 };
        }
    }
}
pub(super) fn colour_line_band(
    data: &[f32],
    w: usize,
    h: usize,
    stride: usize,
    r: usize,
    y0: usize,
    y1: usize,
) -> Vec<(usize, [f32; 3])> {
    let row_of = |y: isize| y.clamp(0, h as isize - 1) as usize;
    let inv_area = 1.0 / ((2 * r + 1) * (2 * r + 1)) as f32;
    let mut pad = vec![0.0f32; w + 2 * r];
    // Rows y0 - 2r .. y1 + 2r: linear RGB (the colour lines) and square-
    // root RGB (the edge and two-colour tests, on the scale of the
    // luminance pass), and the horizontal sums of their means and second
    // moments.
    let ext0 = y0 as isize - 2 * r as isize;
    let ext_rows = (y1 - y0) + 4 * r;
    let mut linear: [Vec<f32>; 3] = std::array::from_fn(|_| vec![0.0f32; ext_rows * w]);
    let mut root: [Vec<f32>; 3] = std::array::from_fn(|_| vec![0.0f32; ext_rows * w]);
    for e in 0..ext_rows {
        let y = row_of(ext0 + e as isize);
        let src = &data[y * w * stride..(y + 1) * w * stride];
        for (x, px) in src.chunks_exact(stride).enumerate() {
            for c in 0..3 {
                linear[c][e * w + x] = px[c].max(0.0);
                root[c][e * w + x] = enc(px[c]);
            }
        }
    }
    let mut hsum: Vec<Vec<f32>> = (0..18).map(|_| vec![0.0f32; ext_rows * w]).collect();
    let mut moment = vec![0.0f32; w];
    for e in 0..ext_rows {
        let span = e * w..(e + 1) * w;
        for (set, channels) in [&linear, &root].into_iter().enumerate() {
            for k in 0..9 {
                if k < 3 {
                    moment.copy_from_slice(&channels[k][span.clone()]);
                } else {
                    let (a, b) = PAIRS[k - 3];
                    for ((m, p), q) in moment
                        .iter_mut()
                        .zip(&channels[a][span.clone()])
                        .zip(&channels[b][span.clone()])
                    {
                        *m = p * q;
                    }
                }
                box_row(&moment, r, &mut pad, &mut hsum[set * 9 + k][span.clone()]);
            }
        }
    }
    // Rows y0 - r .. y1 + r: each window's weight; then, for windows within
    // 2r of a window of some weight (the only ones a changed pixel reads;
    // box sums are exact window sums, so the others may stay 0), its
    // projection onto its line as an affine map per output channel (offset
    // and three slopes); both summed along rows. Mid row m is ext row
    // m + r; its window spans ext rows m ..= m + 2r.
    let mid_rows = (y1 - y0) + 2 * r;
    let mut stats = RowStats::new(w);
    let mut vs: Vec<Vec<f32>> = (0..9).map(|_| vec![0.0f32; w]).collect();
    let mut weights = vec![0.0f32; mid_rows * w];
    let mut columns = Vec::with_capacity(w);
    for m in 0..mid_rows {
        for (k, out) in vs.iter_mut().enumerate() {
            box_column(&hsum[9 + k], w, m, r, out);
        }
        stats.of(&vs, inv_area);
        stats.weights(&mut weights[m * w..(m + 1) * w], &mut columns);
    }
    let weighted: Vec<bool> = weights
        .chunks_exact(w)
        .map(|row| row.iter().any(|&v| v > 0.0))
        .collect();
    let needed = near_some(&weights, w, mid_rows, 2 * r);
    // Per window: the offsets of the three channels, the six entries of the
    // projection's symmetric slope matrix (shrink · axis · axisᵀ, the
    // [`PAIRS`] entries) and the weight.
    let mut csum: Vec<Vec<f32>> = (0..10).map(|_| vec![0.0f32; mid_rows * w]).collect();
    let mut coef: Vec<Vec<f32>> = (0..10).map(|_| vec![0.0f32; w]).collect();
    // The windows' means and axes, for the range test of the pixel each is
    // centred on.
    let mut centred: [Vec<f32>; 6] = std::array::from_fn(|_| vec![0.0f32; mid_rows * w]);
    for m in 0..mid_rows {
        if !weighted[m.saturating_sub(2 * r)..(m + 2 * r + 1).min(mid_rows)].contains(&true) {
            continue;
        }
        for (k, out) in vs.iter_mut().enumerate() {
            box_column(&hsum[k], w, m, r, out);
        }
        stats.of(&vs, inv_area);
        let weights = &weights[m * w..(m + 1) * w];
        columns.clear();
        columns.extend((0..w).filter(|&x| needed[m * w + x]));
        stats.principal(&columns);
        for c in coef.iter_mut() {
            c.fill(0.0);
        }
        for &x in &columns {
            let e = [stats.axis[0][x], stats.axis[1][x], stats.axis[2][x]];
            let mean = [stats.mean[0][x], stats.mean[1][x], stats.mean[2][x]];
            let var = stats.var[x];
            // A window of one colour (a clipped patch) maps to its mean.
            let shrink = var / (var + LINE_EPS);
            let t = shrink * (e[0] * mean[0] + e[1] * mean[1] + e[2] * mean[2]);
            for c in 0..3 {
                coef[c][x] = mean[c] - t * e[c];
            }
            for (k, &(a, b)) in PAIRS.iter().enumerate() {
                coef[3 + k][x] = shrink * e[a] * e[b];
            }
            coef[9][x] = if var > 0.0 { weights[x] } else { 0.0 };
            for c in 0..3 {
                centred[c][m * w + x] = mean[c];
                centred[3 + c][m * w + x] = e[c];
            }
        }
        let span = m * w..(m + 1) * w;
        for (k, sum) in csum.iter_mut().enumerate() {
            box_row(&coef[k], r, &mut pad, &mut sum[span.clone()]);
        }
    }
    let mut changes = Vec::new();
    let mut model: Vec<Vec<f32>> = (0..10).map(|_| vec![0.0f32; w]).collect();
    for y in y0..y1 {
        // Output row y is mid row y - y0 + r (its windows: mid rows y - y0
        // ..= y - y0 + 2r) and ext row y - y0 + 2r.
        let m0 = y - y0;
        if !weighted[m0..=m0 + 2 * r].contains(&true) {
            continue;
        }
        for (k, out) in model.iter_mut().enumerate() {
            box_column(&csum[k], w, m0, r, out);
        }
        let e = y - y0 + 2 * r;
        for x in 0..w {
            // Spread over the window, as in the luminance pass.
            let weight = (2.0 * model[9][x] * inv_area).min(1.0);
            if weight <= 0.0 {
                continue;
            }
            let i = y * w + x;
            let px = &data[i * stride..][..3];
            if px.iter().any(|&c| c < 0.0 || !c.is_finite()) {
                continue;
            }
            let p = [
                linear[0][e * w + x],
                linear[1][e * w + x],
                linear[2][e * w + x],
            ];
            let weight = weight * within_the_mixes(&linear, &centred, w, r, e, m0 + r, x, p);
            if weight <= 0.0 {
                continue;
            }
            // The [`PAIRS`] index of each slope matrix entry.
            const ENTRY: [[usize; 3]; 3] = [[0, 1, 2], [1, 3, 4], [2, 4, 5]];
            let s = |a: usize, b: usize| model[3 + ENTRY[a][b]][x];
            let out = std::array::from_fn(|c| {
                let projected =
                    (model[c][x] + s(c, 0) * p[0] + s(c, 1) * p[1] + s(c, 2) * p[2]) * inv_area;
                (p[c] + weight * (projected - p[c])).max(0.0)
            });
            changes.push((i, out));
        }
    }
    changes
}

/// Misregistration mixes each channel of the two colours by its own share,
/// so it leaves every channel between the two colours' values: how far the
/// pixel `p` (linear RGB, at ext row `e` and column `x`) may move onto its
/// windows' lines, from 1 (inside the channel ranges of the two ends of the
/// line of the window centred on it, mid row `m`, a little beyond) to 0
/// (well outside: a small patch of a third colour on the edge, which can
/// pass the window's two-colour test).
#[allow(clippy::too_many_arguments)]
fn within_the_mixes(
    linear: &[Vec<f32>; 3],
    centred: &[Vec<f32>; 6],
    w: usize,
    r: usize,
    e: usize,
    m: usize,
    x: usize,
    p: [f32; 3],
) -> f32 {
    let mean = [
        centred[0][m * w + x],
        centred[1][m * w + x],
        centred[2][m * w + x],
    ];
    let axis = [
        centred[3][m * w + x],
        centred[4][m * w + x],
        centred[5][m * w + x],
    ];
    if axis == [0.0; 3] {
        return 1.0;
    }
    // The line's ends: the window's extreme pixels along it.
    let (mut low, mut high) = (f32::MAX, f32::MIN);
    for row in e - r..=e + r {
        for dx in 0..=2 * r {
            let xx = (x + dx).saturating_sub(r).min(w - 1);
            let t = (0..3)
                .map(|c| axis[c] * (linear[c][row * w + xx] - mean[c]))
                .sum::<f32>();
            low = low.min(t);
            high = high.max(t);
        }
    }
    let length = high - low;
    if length.is_nan() || length <= 0.0 {
        return 1.0;
    }
    let beyond = (0..3)
        .map(|c| {
            let (a, b) = (mean[c] + low * axis[c], mean[c] + high * axis[c]);
            (a.min(b) - p[c]).max(p[c] - a.max(b)).max(0.0)
        })
        .fold(0.0f32, f32::max)
        / length;
    ((MIX_NONE - beyond) / (MIX_NONE - MIX_FULL)).clamp(0.0, 1.0)
}
