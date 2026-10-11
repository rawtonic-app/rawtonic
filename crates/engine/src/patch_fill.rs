//! Content-aware fill of a hole from the photo's own pixels: the multi-scale
//! patch completion of Wexler, Shechtman and Irani ("Space-Time Completion
//! of Video", IEEE TPAMI 2007) with the nearest-neighbour field found by
//! PatchMatch (Barnes, Shechtman, Finkelstein and Goldman, SIGGRAPH 2009),
//! and the two measures of Newson, Almansa, Fradet, Gousseau and Pérez
//! ("Video Inpainting of Complex Scenes", SIAM J. Imaging Sciences 2014)
//! that keep it from washing textured areas out into smooth ones:
//!
//! - texture features (the local mean of the absolute luminance gradients)
//!   are part of every patch, so a textured hole does not match a smooth
//!   patch merely because the start of the hole is smooth;
//! - the coarsest level starts with an onion-peel fill (ring by ring from
//!   the edge, each pixel from the best match of its known neighbourhood)
//!   and every finer level starts from the match field carried up, not from
//!   a blurred copy of the coarser result.
//!
//! Every patch that overlaps the hole is matched to the most similar patch
//! of known pixels, and each hole pixel becomes the similarity-weighted vote
//! of the matches covering it (the finest level takes the best match's pixel,
//! which keeps fine texture). An optional *guide* (a learned inpainting
//! model's fill) starts the coarsest level instead and enters the patch
//! distance with a weight.
//!
//! Deterministic: a fixed-seed generator per row band, the same result on
//! any number of threads.

use rayon::prelude::*;

/// Settings of [`fill`].
#[derive(Clone, Debug, PartialEq)]
pub struct PatchFillParams {
    /// Patches are `2 r + 1` pixels square.
    pub patch_radius: usize,
    /// EM iterations at the coarsest level; finer levels halve them down to
    /// `min_em_iterations`.
    pub em_iterations: usize,
    pub min_em_iterations: usize,
    /// PatchMatch passes per EM iteration.
    pub pm_iterations: usize,
    /// The coarsest level: the hole at most this many of its pixels across.
    pub coarsest_extent: usize,
    /// Levels where the hole is wider than this many of their pixels (the
    /// finest ones of a large hole) run one EM iteration with one local
    /// PatchMatch pass: the structure is settled by then, the matches
    /// carried up only need refining. 0: every level runs in full.
    pub fine_extent: usize,
    /// Weight of the texture features in the patch distance.
    pub texture_weight: f32,
    /// Weight of the guide in the patch distance (hole pixels only).
    pub guide_weight: f32,
    /// The finest level takes each hole pixel from its best match instead
    /// of the weighted mean of all matches covering it.
    pub sharp_finish: bool,
    pub seed: u64,
}

impl Default for PatchFillParams {
    fn default() -> Self {
        Self {
            patch_radius: 3,
            em_iterations: 10,
            min_em_iterations: 2,
            pm_iterations: 2,
            coarsest_extent: 24,
            fine_extent: 0,
            texture_weight: 1.0,
            guide_weight: 1.0,
            sharp_finish: true,
            seed: 1,
        }
    }
}

type Px = [f32; 3];
/// A row band of the match field and its costs, with its index.
type Band<'a> = (usize, (&'a mut [(i32, i32)], &'a mut [f32]));
/// Colour and the two texture features.
type Fx = [f32; 5];

/// SplitMix64.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed ^ 0x9E37_79B9_7F4A_7C15)
    }
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    /// Uniform in `-r..=r`.
    fn offset(&mut self, r: i32) -> i32 {
        (self.next() % (2 * r as u64 + 1)) as i32 - r
    }
}

struct Level {
    w: usize,
    h: usize,
    img: Vec<Px>,
    hole: Vec<bool>,
    guide: Option<Vec<Px>>,
    /// Pixels whose patches may serve as sources (all known ones without a
    /// source mask).
    source: Vec<bool>,
    /// Texture features of the finest level, averaged down (with a guide,
    /// of the guide's fill in the hole).
    tex: Vec<[f32; 2]>,
}

fn downsample(l: &Level) -> Level {
    let (w, h) = (l.w.div_ceil(2), l.h.div_ceil(2));
    let mut img = vec![[0.0; 3]; w * h];
    let mut hole = vec![false; w * h];
    let mut guide = l.guide.as_ref().map(|_| vec![[0.0; 3]; w * h]);
    let mut tex = vec![[0.0f32; 2]; w * h];
    let mut source = vec![false; w * h];
    let guided = l.guide.is_some();
    for y in 0..h {
        for x in 0..w {
            let (mut tsum, mut tn) = ([0.0f32; 2], 0.0f32);
            let (mut sum, mut n) = ([0.0f32; 3], 0.0f32);
            let (mut gsum, mut gn) = ([0.0f32; 3], 0.0f32);
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let (sx, sy) = (2 * x + dx, 2 * y + dy);
                if sx >= l.w || sy >= l.h {
                    continue;
                }
                let i = sy * l.w + sx;
                source[y * w + x] |= l.source[i];
                if !l.hole[i] || guided {
                    tsum[0] += l.tex[i][0];
                    tsum[1] += l.tex[i][1];
                    tn += 1.0;
                }
                if l.hole[i] {
                    hole[y * w + x] = true;
                } else {
                    (0..3).for_each(|c| sum[c] += l.img[i][c]);
                    n += 1.0;
                }
                if let Some(g) = &l.guide {
                    (0..3).for_each(|c| gsum[c] += g[i][c]);
                    gn += 1.0;
                }
            }
            if n > 0.0 {
                img[y * w + x] = sum.map(|v| v / n);
            }
            if tn > 0.0 {
                tex[y * w + x] = tsum.map(|v| v / tn);
            }
            if let Some(g) = guide.as_mut() {
                g[y * w + x] = gsum.map(|v| v / gn.max(1.0));
            }
        }
    }
    Level {
        w,
        h,
        img,
        hole,
        guide,
        source,
        tex,
    }
}

/// Summed-area table of `m` (`(w + 1) × (h + 1)`).
fn integral(w: usize, h: usize, m: &[bool]) -> Vec<u32> {
    let mut s = vec![0u32; (w + 1) * (h + 1)];
    for y in 0..h {
        let mut row = 0u32;
        for x in 0..w {
            row += u32::from(m[y * w + x]);
            s[(y + 1) * (w + 1) + x + 1] = s[y * (w + 1) + x + 1] + row;
        }
    }
    s
}

/// Set cells of the summed-area table `s` in `[x0, x1) × [y0, y1)`, clipped.
fn count(s: &[u32], w: usize, h: usize, x0: i32, y0: i32, x1: i32, y1: i32) -> u32 {
    let (x0, y0) = (x0.max(0) as usize, y0.max(0) as usize);
    let (x1, y1) = ((x1.max(0) as usize).min(w), (y1.max(0) as usize).min(h));
    if x1 <= x0 || y1 <= y0 {
        return 0;
    }
    let sw = w + 1;
    s[y1 * sw + x1] + s[y0 * sw + x0] - s[y0 * sw + x1] - s[y1 * sw + x0]
}

fn hole_extent(w: usize, h: usize, hole: &[bool]) -> Option<usize> {
    let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
    for y in 0..h {
        for x in 0..w {
            if hole[y * w + x] {
                x0 = x0.min(x);
                x1 = x1.max(x);
                y0 = y0.min(y);
                y1 = y1.max(y);
            }
        }
    }
    (x0 != usize::MAX).then(|| (x1 - x0 + 1).max(y1 - y0 + 1))
}

/// Texture features of the known pixels: the mean absolute horizontal and
/// vertical luminance gradients over a 5 × 5 window of known pixels, times
/// `weight`. Hole pixels get zero (they are filled with the colour).
fn texture_features(w: usize, h: usize, img: &[Px], known: &[bool], weight: f32) -> Vec<[f32; 2]> {
    let luma = |p: Px| 0.299 * p[0] + 0.587 * p[1] + 0.114 * p[2];
    let mut g = vec![[0.0f32; 2]; w * h];
    let mut ok = vec![false; w * h];
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            if !known[i] {
                continue;
            }
            let (xl, xr) = (x.saturating_sub(1), (x + 1).min(w - 1));
            let (yu, yd) = (y.saturating_sub(1), (y + 1).min(h - 1));
            if known[y * w + xl] && known[y * w + xr] && known[yu * w + x] && known[yd * w + x] {
                let gx =
                    (luma(img[y * w + xr]) - luma(img[y * w + xl])).abs() / (xr - xl).max(1) as f32;
                let gy =
                    (luma(img[yd * w + x]) - luma(img[yu * w + x])).abs() / (yd - yu).max(1) as f32;
                g[i] = [gx, gy];
                ok[i] = true;
            }
        }
    }
    let r = 2i32;
    (0..w * h)
        .into_par_iter()
        .map(|i| {
            if !known[i] {
                return [0.0; 2];
            }
            let (x, y) = ((i % w) as i32, (i / w) as i32);
            let (mut s, mut n) = ([0.0f32; 2], 0.0f32);
            for dy in -r..=r {
                for dx in -r..=r {
                    let (xx, yy) = (x + dx, y + dy);
                    if xx < 0 || yy < 0 || xx >= w as i32 || yy >= h as i32 {
                        continue;
                    }
                    let j = yy as usize * w + xx as usize;
                    if ok[j] {
                        s[0] += g[j][0];
                        s[1] += g[j][1];
                        n += 1.0;
                    }
                }
            }
            if n > 0.0 {
                [weight * s[0] / n, weight * s[1] / n]
            } else {
                [0.0; 2]
            }
        })
        .collect()
}

fn with_features(img: &[Px], tex: &[[f32; 2]]) -> Vec<Fx> {
    img.iter()
        .zip(tex)
        .map(|(p, t)| [p[0], p[1], p[2], t[0], t[1]])
        .collect()
}

#[inline]
fn d2(a: &Fx, b: &Fx) -> f32 {
    (a[0] - b[0]).powi(2)
        + (a[1] - b[1]).powi(2)
        + (a[2] - b[2]).powi(2)
        + (a[3] - b[3]).powi(2)
        + (a[4] - b[4]).powi(2)
}

struct Solver<'a> {
    w: usize,
    h: usize,
    r: i32,
    img: &'a [Fx],
    hole: &'a [bool],
    guide: Option<&'a [Px]>,
    guide_weight: f32,
    /// Source centres: the whole patch inside and free of the hole.
    valid: &'a [bool],
    /// Target centres: the patch overlaps the hole.
    targets: &'a [bool],
}

impl Solver<'_> {
    #[inline]
    fn is_valid(&self, x: i32, y: i32) -> bool {
        x >= 0
            && y >= 0
            && (x as usize) < self.w
            && (y as usize) < self.h
            && self.valid[y as usize * self.w + x as usize]
    }

    /// Mean squared difference between the target patch at `t` (clipped to
    /// the image; with `known`, only its known pixels) and the source patch
    /// at `s`, with the guide's term on hole pixels; infinite once it cannot
    /// beat `cutoff`.
    fn dist(&self, t: (i32, i32), s: (i32, i32), cutoff: f32, known: Option<&[bool]>) -> f32 {
        let (w, h, r) = (self.w as i32, self.h as i32, self.r);
        let full = ((2 * r + 1) * (2 * r + 1)) as f32;
        let bound = cutoff * full;
        let mut sum = 0.0f32;
        let mut n = 0u32;
        for dy in -r..=r {
            let ty = t.1 + dy;
            if ty < 0 || ty >= h {
                continue;
            }
            let trow = ty as usize * self.w;
            let srow = (s.1 + dy) as usize * self.w;
            for dx in -r..=r {
                let tx = t.0 + dx;
                if tx < 0 || tx >= w {
                    continue;
                }
                let ti = trow + tx as usize;
                if known.is_some_and(|k| !k[ti]) {
                    continue;
                }
                let sv = &self.img[srow + (s.0 + dx) as usize];
                let mut d = d2(&self.img[ti], sv);
                if let Some(g) = self.guide
                    && self.hole[ti]
                {
                    let gv = g[ti];
                    d += self.guide_weight
                        * ((gv[0] - sv[0]).powi(2)
                            + (gv[1] - sv[1]).powi(2)
                            + (gv[2] - sv[2]).powi(2));
                }
                sum += d;
                n += 1;
            }
            if sum > bound {
                return f32::INFINITY;
            }
        }
        if n == 0 {
            f32::INFINITY
        } else {
            sum / n as f32
        }
    }

    /// One PatchMatch pass, in parallel bands of rows (propagation stays
    /// within a band), alternating scan order between passes.
    fn patchmatch(
        &self,
        nnf: &mut [(i32, i32)],
        cost: &mut [f32],
        pass: usize,
        seed: u64,
        radius: i32,
    ) {
        const BAND: usize = 16;
        let w = self.w;
        let reverse = pass % 2 == 1;
        let band = |(bi, (nn, co)): Band<'_>| {
            let mut rng = Rng::new(
                seed ^ (bi as u64).wrapping_mul(0x2545_F491_4F6C_DD1D) ^ ((pass as u64) << 40),
            );
            let rows = nn.len() / w;
            let step: i32 = if reverse { -1 } else { 1 };
            for ry in 0..rows {
                let ly = if reverse { rows - 1 - ry } else { ry };
                let y = bi * BAND + ly;
                for rx in 0..w {
                    let x = if reverse { w - 1 - rx } else { rx };
                    if !self.targets[y * w + x] {
                        continue;
                    }
                    let li = ly * w + x;
                    let t = (x as i32, y as i32);
                    let (mut best, mut bc) = (nn[li], co[li]);
                    let consider = |cand: (i32, i32), best: &mut (i32, i32), bc: &mut f32| {
                        if cand != *best && self.is_valid(cand.0, cand.1) {
                            let d = self.dist(t, cand, *bc, None);
                            if d < *bc {
                                *best = cand;
                                *bc = d;
                            }
                        }
                    };
                    let nx = x as i32 - step;
                    if nx >= 0 && (nx as usize) < w && self.targets[y * w + nx as usize] {
                        let c = nn[ly * w + nx as usize];
                        consider((c.0 + step, c.1), &mut best, &mut bc);
                    }
                    let nly = ly as i32 - step;
                    if nly >= 0
                        && (nly as usize) < rows
                        && self.targets[(bi * BAND + nly as usize) * w + x]
                    {
                        let c = nn[nly as usize * w + x];
                        consider((c.0, c.1 + step), &mut best, &mut bc);
                    }
                    let mut rad = radius;
                    while rad >= 1 {
                        let cand = (best.0 + rng.offset(rad), best.1 + rng.offset(rad));
                        consider(cand, &mut best, &mut bc);
                        rad /= 2;
                    }
                    nn[li] = best;
                    co[li] = bc;
                }
            }
        };
        nnf.par_chunks_mut(BAND * w)
            .zip(cost.par_chunks_mut(BAND * w))
            .enumerate()
            .for_each(band);
    }

    /// New values of the hole pixels from the matches covering them: their
    /// weighted mean (Wexler et al.'s vote; equal weights without costs), or
    /// with `sharp` the value of the best match.
    fn vote(&self, nnf: &[(i32, i32)], cost: Option<&[f32]>, sharp: bool) -> Vec<(usize, Fx)> {
        let (w, h, r) = (self.w, self.h, self.r);
        // σ² from the 75th percentile of the match costs.
        let sigma2 = cost.map_or(1.0, |cost| {
            let mut costs: Vec<f32> = cost
                .iter()
                .zip(self.targets)
                .filter(|(c, t)| **t && c.is_finite())
                .map(|(c, _)| *c)
                .collect();
            if costs.is_empty() {
                1.0
            } else {
                let k = (costs.len() * 3 / 4).min(costs.len() - 1);
                let (_, v, _) = costs.select_nth_unstable_by(k, |a, b| a.total_cmp(b));
                v.max(1e-8)
            }
        });
        (0..h)
            .into_par_iter()
            .flat_map_iter(|y| {
                let mut out = Vec::new();
                for x in 0..w {
                    if !self.hole[y * w + x] {
                        continue;
                    }
                    let mut acc = [0.0f32; 5];
                    let mut wsum = 0.0f32;
                    let mut best = (f32::INFINITY, [0.0f32; 5]);
                    for dy in -r..=r {
                        let ty = y as i32 + dy;
                        if ty < 0 || ty as usize >= h {
                            continue;
                        }
                        for dx in -r..=r {
                            let tx = x as i32 + dx;
                            if tx < 0 || tx as usize >= w {
                                continue;
                            }
                            let ti = ty as usize * w + tx as usize;
                            let c = cost.map_or(0.0, |c| c[ti]);
                            if !self.targets[ti] || !c.is_finite() {
                                continue;
                            }
                            let s = nnf[ti];
                            let v = self.img[(s.1 - dy) as usize * w + (s.0 - dx) as usize];
                            if sharp {
                                if c < best.0 {
                                    best = (c, v);
                                }
                            } else {
                                let wt = (-c / (2.0 * sigma2)).exp().max(1e-12);
                                (0..5).for_each(|k| acc[k] += v[k] * wt);
                                wsum += wt;
                            }
                        }
                    }
                    if sharp && best.0.is_finite() {
                        out.push((y * w + x, best.1));
                    } else if wsum > 0.0 {
                        out.push((y * w + x, acc.map(|v| v / wsum)));
                    }
                }
                out
            })
            .collect()
    }

    /// Onion-peel start: the hole filled ring by ring from its edge, each
    /// pixel from the centre of the source whose patch best matches the
    /// pixel's known neighbourhood (all sources searched, or `budget` of
    /// them at random when there are more).
    fn onion_peel(&self, img: &mut [Fx], valid_list: &[(i32, i32)], budget: usize, seed: u64) {
        let (w, h) = (self.w, self.h);
        let mut known: Vec<bool> = self.hole.iter().map(|&x| !x).collect();
        let mut rng = Rng::new(seed);
        let sources: Vec<(i32, i32)> = if valid_list.len() <= budget {
            valid_list.to_vec()
        } else {
            (0..budget)
                .map(|_| valid_list[(rng.next() % valid_list.len() as u64) as usize])
                .collect()
        };
        loop {
            let ring: Vec<usize> = (0..w * h)
                .filter(|&i| {
                    if known[i] {
                        return false;
                    }
                    let (x, y) = ((i % w) as i32, (i / w) as i32);
                    (-1..=1).any(|dy| {
                        (-1..=1).any(|dx| {
                            let (xx, yy) = (x + dx, y + dy);
                            xx >= 0
                                && yy >= 0
                                && xx < w as i32
                                && yy < h as i32
                                && known[yy as usize * w + xx as usize]
                        })
                    })
                })
                .collect();
            if ring.is_empty() {
                break;
            }
            let view = Solver {
                img: &*img,
                ..*self
            };
            let picks: Vec<(usize, Fx)> = ring
                .par_iter()
                .map(|&i| {
                    let t = ((i % w) as i32, (i / w) as i32);
                    let mut best = (f32::INFINITY, sources[0]);
                    for &s in &sources {
                        let d = view.dist(t, s, best.0, Some(&known));
                        if d < best.0 {
                            best = (d, s);
                        }
                    }
                    let s = best.1;
                    (i, img[s.1 as usize * w + s.0 as usize])
                })
                .collect();
            for (i, v) in picks {
                img[i] = v;
                known[i] = true;
            }
        }
    }
}

/// Fills the pixels of `image` (`width × height`, RGB, values about 0–1)
/// where `hole` is set from the rest of the image; returns the image with
/// the hole filled and everything else unchanged. `guide`, when given, is a
/// fill of the same size whose structure the result follows.
pub fn fill(
    width: usize,
    height: usize,
    image: &[Px],
    hole: &[bool],
    guide: Option<&[Px]>,
    params: &PatchFillParams,
) -> Vec<Px> {
    fill_from(width, height, image, hole, None, guide, params)
}

/// [`fill`] taking its source patches only where `source` is set (their
/// centres; known pixels elsewhere still frame the hole and its patches).
pub fn fill_from(
    width: usize,
    height: usize,
    image: &[Px],
    hole: &[bool],
    source: Option<&[bool]>,
    guide: Option<&[Px]>,
    params: &PatchFillParams,
) -> Vec<Px> {
    assert_eq!(image.len(), width * height);
    assert_eq!(hole.len(), width * height);
    if let Some(src) = source {
        assert_eq!(src.len(), width * height);
    }
    if let Some(g) = guide {
        assert_eq!(g.len(), width * height);
    }
    let Some(extent) = hole_extent(width, height, hole) else {
        return image.to_vec();
    };
    let r = params.patch_radius.max(1);
    let psz = 2 * r + 1;
    let tex = match guide {
        Some(g) => {
            let filled: Vec<Px> = (0..width * height)
                .map(|i| if hole[i] { g[i] } else { image[i] })
                .collect();
            texture_features(
                width,
                height,
                &filled,
                &vec![true; width * height],
                params.texture_weight,
            )
        }
        None => {
            let known: Vec<bool> = hole.iter().map(|&x| !x).collect();
            texture_features(width, height, image, &known, params.texture_weight)
        }
    };
    let mut levels = vec![Level {
        w: width,
        h: height,
        img: image.to_vec(),
        hole: hole.to_vec(),
        guide: guide.map(<[Px]>::to_vec),
        source: source.map_or_else(|| vec![true; width * height], <[bool]>::to_vec),
        tex,
    }];
    let mut extent_now = extent;
    let mut extents = vec![extent];
    let coarsest = params.coarsest_extent.max(2 * psz);
    while let Some(l) = levels.last() {
        if extent_now <= coarsest || l.w / 2 < 3 * psz || l.h / 2 < 3 * psz {
            break;
        }
        let d = downsample(l);
        levels.push(d);
        extent_now = extent_now.div_ceil(2);
        extents.push(extent_now);
    }
    let n_levels = levels.len();
    let ri = r as i32;
    let mut nnf: Vec<(i32, i32)> = Vec::new();
    let mut prev_w = 0usize;
    let mut result: Option<Vec<Fx>> = None;
    for li in (0..n_levels).rev() {
        let level = &levels[li];
        let (lw, lh) = (level.w, level.h);
        let hs = integral(lw, lh, &level.hole);
        let mut valid = vec![false; lw * lh];
        let mut valid_list = Vec::new();
        let mut targets = vec![false; lw * lh];
        for y in 0..lh as i32 {
            for x in 0..lw as i32 {
                let c = count(&hs, lw, lh, x - ri, y - ri, x + ri + 1, y + ri + 1);
                let inside = x >= ri && y >= ri && x + ri < lw as i32 && y + ri < lh as i32;
                let i = y as usize * lw + x as usize;
                if inside && c == 0 && level.source[i] {
                    valid[i] = true;
                    valid_list.push((x, y));
                }
                targets[i] = c > 0;
            }
        }
        let mut img = match &level.guide {
            // The guide's fill in the hole.
            Some(g) if result.is_none() => {
                let filled: Vec<Px> = (0..lw * lh)
                    .map(|i| if level.hole[i] { g[i] } else { level.img[i] })
                    .collect();
                with_features(&filled, &level.tex)
            }
            _ => with_features(&level.img, &level.tex),
        };
        if valid_list.is_empty() {
            result = Some(img);
            nnf.clear();
            prev_w = lw;
            continue;
        }
        let mut rng = Rng::new(params.seed ^ ((li as u64) << 20));
        // The match field: carried up from the coarser level where it lands
        // on a valid source, random elsewhere.
        let mut field = vec![(0i32, 0i32); lw * lh];
        for y in 0..lh {
            for x in 0..lw {
                if !targets[y * lw + x] {
                    continue;
                }
                let mut cand = None;
                if !nnf.is_empty()
                    && let Some(&(sx, sy)) = nnf.get((y / 2) * prev_w + x / 2)
                {
                    let c = (sx * 2 + (x % 2) as i32, sy * 2 + (y % 2) as i32);
                    if c.0 >= 0
                        && c.1 >= 0
                        && (c.0 as usize) < lw
                        && (c.1 as usize) < lh
                        && valid[c.1 as usize * lw + c.0 as usize]
                    {
                        cand = Some(c);
                    }
                }
                field[y * lw + x] = cand
                    .unwrap_or_else(|| valid_list[(rng.next() % valid_list.len() as u64) as usize]);
            }
        }
        let carried = !nnf.is_empty();
        nnf = field;
        {
            let s = Solver {
                w: lw,
                h: lh,
                r: ri,
                img: &img,
                hole: &level.hole,
                guide: level.guide.as_deref(),
                guide_weight: params.guide_weight,
                valid: &valid,
                targets: &targets,
            };
            if carried {
                // The finer start: the carried-up matches voted with equal
                // weights (coherent copies, not a blurred coarser image).
                let updates = s.vote(&nnf, None, false);
                for (i, v) in updates {
                    img[i] = v;
                }
            } else if level.guide.is_none() {
                let mut start = img.clone();
                s.onion_peel(&mut start, &valid_list, 4096, params.seed);
                img = start;
            }
        }
        let fine = carried && params.fine_extent > 0 && extents[li] > params.fine_extent;
        let em = if li + 1 == n_levels {
            params.em_iterations
        } else if fine {
            1
        } else {
            (params.em_iterations >> (n_levels - 1 - li)).max(params.min_em_iterations)
        };
        let pm_iterations = if fine { 1 } else { params.pm_iterations };
        let full_search = lw.max(lh) as i32;
        let mut cost = vec![f32::INFINITY; lw * lh];
        for it in 0..em {
            let s = Solver {
                w: lw,
                h: lh,
                r: ri,
                img: &img,
                hole: &level.hole,
                guide: level.guide.as_deref(),
                guide_weight: params.guide_weight,
                valid: &valid,
                targets: &targets,
            };
            // The hole changed with the last vote: refresh the costs.
            cost.par_iter_mut().enumerate().for_each(|(i, c)| {
                if s.targets[i] {
                    *c = s.dist(
                        ((i % lw) as i32, (i / lw) as i32),
                        nnf[i],
                        f32::INFINITY,
                        None,
                    );
                }
            });
            let radius = if fine {
                (2 * psz as i32).min(full_search)
            } else if li + 1 == n_levels || (it == 0 && !carried) {
                full_search
            } else {
                (4 * psz as i32).min(full_search)
            };
            for pass in 0..pm_iterations {
                s.patchmatch(
                    &mut nnf,
                    &mut cost,
                    pass + it * pm_iterations,
                    params.seed.wrapping_add((li * 1000 + it) as u64),
                    radius,
                );
            }
            let sharp = params.sharp_finish && li == 0 && it + 1 == em;
            let updates = s.vote(&nnf, Some(&cost), sharp);
            for (i, v) in updates {
                img[i] = v;
            }
        }
        prev_w = lw;
        result = Some(img);
    }
    result
        .map(|img| img.iter().map(|p| [p[0], p[1], p[2]]).collect())
        .unwrap_or_else(|| image.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noise(seed: u64) -> impl FnMut() -> f32 {
        let mut rng = Rng::new(seed);
        move || (rng.next() % 1000) as f32 / 1000.0 - 0.5
    }

    fn disc(w: usize, h: usize, cx: f32, cy: f32, r: f32) -> Vec<bool> {
        (0..w * h)
            .map(|i| ((i % w) as f32 - cx).hypot((i / w) as f32 - cy) < r)
            .collect()
    }

    fn hole_rmse(a: &[Px], b: &[Px], hole: &[bool]) -> f32 {
        let (mut e, mut n) = (0.0f32, 0);
        for (i, &hh) in hole.iter().enumerate() {
            if hh {
                (0..3).for_each(|c| e += (a[i][c] - b[i][c]).powi(2));
                n += 3;
            }
        }
        (e / n as f32).sqrt()
    }

    #[test]
    fn fills_a_blemish_from_the_texture_and_keeps_the_rest() {
        let (w, h) = (120, 100);
        let mut n = noise(7);
        let clean: Vec<Px> = (0..w * h)
            .map(|i| {
                let (x, y) = ((i % w) as f32, (i / w) as f32);
                let g = 0.04 * n();
                [0.3 + 0.003 * x + g, 0.5 + 0.002 * y + g, 0.4 + g]
            })
            .collect();
        let hole = disc(w, h, 60.0, 50.0, 12.0);
        let dirty: Vec<Px> = clean
            .iter()
            .zip(&hole)
            .map(|(&c, &hh)| if hh { [0.02; 3] } else { c })
            .collect();
        let params = PatchFillParams::default();
        let out = fill(w, h, &dirty, &hole, None, &params);
        let before = hole_rmse(&dirty, &clean, &hole);
        let after = hole_rmse(&out, &clean, &hole);
        assert!(after < 0.06 && after < 0.15 * before, "{before} -> {after}");
        for i in 0..w * h {
            if !hole[i] {
                assert_eq!(out[i], dirty[i]);
            }
        }
        // Deterministic.
        assert_eq!(out, fill(w, h, &dirty, &hole, None, &params));
    }

    #[test]
    fn takes_sources_only_where_allowed() {
        // Left half dark, right half bright, hole in the left half: with
        // sources limited to the right half the fill is bright.
        let (w, h) = (96, 64);
        let img: Vec<Px> = (0..w * h)
            .map(|i| if i % w < w / 2 { [0.2; 3] } else { [0.8; 3] })
            .collect();
        let hole = disc(w, h, 24.0, 32.0, 8.0);
        let right: Vec<bool> = (0..w * h).map(|i| i % w >= w / 2 + 6).collect();
        let params = PatchFillParams::default();
        let mean = |out: &[Px]| {
            let v: Vec<f32> = (0..w * h).filter(|&i| hole[i]).map(|i| out[i][0]).collect();
            v.iter().sum::<f32>() / v.len() as f32
        };
        assert!(mean(&fill(w, h, &img, &hole, None, &params)) < 0.3);
        assert!(mean(&fill_from(w, h, &img, &hole, Some(&right), None, &params)) > 0.7);
    }

    #[test]
    fn continues_stripes_and_keeps_their_contrast() {
        // Vertical stripes 4 px wide: a fill must keep them, not blur them grey.
        let (w, h) = (96, 96);
        let stripe = |x: usize| if (x / 4).is_multiple_of(2) { 0.2 } else { 0.8 };
        let clean: Vec<Px> = (0..w * h).map(|i| [stripe(i % w); 3]).collect();
        let hole = disc(w, h, 48.0, 48.0, 10.0);
        let dirty: Vec<Px> = clean
            .iter()
            .zip(&hole)
            .map(|(&c, &hh)| if hh { [0.5; 3] } else { c })
            .collect();
        let out = fill(w, h, &dirty, &hole, None, &PatchFillParams::default());
        assert!(hole_rmse(&out, &clean, &hole) < 0.12);
        let dev: f32 = (0..w * h)
            .filter(|&i| hole[i])
            .map(|i| (out[i][0] - 0.5).abs())
            .sum::<f32>()
            / hole.iter().filter(|&&x| x).count() as f32;
        assert!(dev > 0.22, "mean deviation from grey {dev}");
    }

    #[test]
    fn keeps_texture_in_a_large_hole_beside_a_smooth_area() {
        // Left: smooth grey; right: strong grain. A large hole in the grain
        // must be filled with grain, not with the smooth grey.
        let (w, h) = (240, 160);
        let mut n = noise(11);
        let clean: Vec<Px> = (0..w * h)
            .map(|i| {
                if i % w < 80 {
                    [0.5; 3]
                } else {
                    [0.5 + 0.4 * n(); 3]
                }
            })
            .collect();
        let hole = disc(w, h, 160.0, 80.0, 40.0);
        let out = fill(w, h, &clean, &hole, None, &PatchFillParams::default());
        let inside: Vec<f32> = (0..w * h).filter(|&i| hole[i]).map(|i| out[i][0]).collect();
        let mean = inside.iter().sum::<f32>() / inside.len() as f32;
        let sd =
            (inside.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / inside.len() as f32).sqrt();
        // The grain's own deviation is 0.4 / sqrt(12) ≈ 0.115.
        assert!(sd > 0.08, "deviation in the hole {sd}");
    }

    #[test]
    fn follows_the_guide_where_the_texture_is_ambiguous() {
        let (w, h) = (120, 90);
        let mut n = noise(3);
        let clean: Vec<Px> = (0..w * h)
            .map(|i| {
                let v = if i % w < 60 { 0.25 } else { 0.75 } + 0.05 * n();
                [v; 3]
            })
            .collect();
        let hole: Vec<bool> = (0..w * h)
            .map(|i| (40..80).contains(&(i % w)) && (25..65).contains(&(i / w)))
            .collect();
        let guide: Vec<Px> = (0..w * h)
            .map(|i| {
                let x = (i % w) as f32;
                [0.25 + 0.5 / (1.0 + (-(x - 59.5) / 3.0).exp()); 3]
            })
            .collect();
        let out = fill(
            w,
            h,
            &clean,
            &hole,
            Some(&guide),
            &PatchFillParams::default(),
        );
        for y in 30..60 {
            let left: f32 = (45..55).map(|x| out[y * w + x][0]).sum::<f32>() / 10.0;
            let right: f32 = (65..75).map(|x| out[y * w + x][0]).sum::<f32>() / 10.0;
            assert!(left < 0.4 && right > 0.6, "row {y}: {left} {right}");
        }
    }

    #[test]
    fn an_empty_hole_returns_the_image() {
        let img = vec![[0.5f32; 3]; 64];
        let out = fill(8, 8, &img, &[false; 64], None, &PatchFillParams::default());
        assert_eq!(out, img);
    }
}
