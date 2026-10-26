//! The grain tile of `grainVersion` 3 (shader `apply_film_grain`; notes:
//! reference/grain.md "Film grain (grainVersion 3)").
//!
//! Film grain is silver or dye grains in each emulsion layer: a Boolean
//! model (Newson, Faraj, Delon, Galerne, "Realistic Film Grain Rendering",
//! IPOL 2017) puts grains as disks with random radii at random places, and a
//! layer of normalised density `u` is covered where any developed grain lies.
//! Every grain here also has a sensitivity `t` (uniform in 0–1) and is
//! developed when `t` is below the level's threshold, so the grains of a
//! lighter level are a subset of a denser level's — one realisation serves
//! every density, and the shader can interpolate between neighbouring
//! levels. Per level the tile holds the covered share of each texel after a
//! small optical blur, minus the level's mean (the grain's fluctuation), with
//! box-filtered mips so a pixel reads the average over its footprint: the
//! grain looks the same at every zoom as the full-size image scaled down.
//!
//! The tile is computed on the CPU with only additions, multiplications,
//! divisions and comparisons (no library `exp`/`ln`), so it is bit-identical
//! on every platform; the shader's interpolation is then the only place
//! GPUs can differ, by rounding.

use rayon::prelude::*;

/// Texels per tile side (a power of two; the tile repeats, the shader blends
/// randomly shifted copies so the repeat does not show).
pub const TILE: usize = 512;
/// Density levels stored per texel (the empty level 0 is implicit).
pub const LEVELS: usize = 12;
/// Normalised density of the densest level.
pub const U_MAX: f64 = 0.95;
/// Mean grain radius in texels.
const RADIUS: f64 = 1.2;
/// Standard deviation of the log grain radius.
const RADIUS_SIGMA: f64 = 0.5;
/// Sub-samples per texel side for the covered share.
const SUPERSAMPLE: usize = 6;
/// Number of grains: λ T² with λ = −ln(1 − U_MAX) / (π E[r²]),
/// E[r²] = RADIUS² exp(RADIUS_SIGMA²) (a fixed count instead of a Poisson
/// draw: the same statistics on a tile this size).
const GRAINS: usize = 135_194;
/// Development thresholds of the levels k = 1..LEVELS: a grain with
/// sensitivity t is developed at level k when t < ln(1 − u_k) / ln(1 − U_MAX),
/// u_k = U_MAX k / LEVELS, so the expected covered share is u_k.
const THRESHOLDS: [f64; LEVELS] = [
    0.027_531_239_207_298_28,
    0.057_538_928_782_942_86,
    0.090_513_018_434_350_25,
    0.127_104_981_603_066_63,
    0.168_207_681_764_434_96,
    0.215_091_656_246_731_1,
    0.269_653_632_272_599_14,
    0.334_910_471_713_654_3,
    0.416_102_743_809_581_7,
    0.523_616_857_140_880_6,
    0.683_189_127_721_616_2,
    1.0,
];
/// Rows of texels stamped per parallel band.
const BAND: usize = 16;

/// SplitMix64: a small generator with a fixed sequence on every platform.
struct SplitMix(u64);

impl SplitMix {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in [0, 1).
    fn uniform(&mut self) -> f64 {
        (self.next() >> 11) as f64 * (1.0 / (1u64 << 53) as f64)
    }

    /// Approximately standard normal (sum of twelve uniforms).
    fn normal(&mut self) -> f64 {
        (0..12).map(|_| self.uniform()).sum::<f64>() - 6.0
    }
}

/// exp(y) for |y| < 4 from additions and multiplications only (relative
/// error below 1e-9), so radii are the same on every platform.
fn exp_portable(y: f64) -> f64 {
    let x = y / 16.0;
    let mut term = 1.0;
    let mut sum = 1.0;
    for k in 1..=9 {
        term = term * x / k as f64;
        sum += term;
    }
    let mut r = sum;
    for _ in 0..4 {
        r *= r;
    }
    r
}

struct Grain {
    x: f64,
    y: f64,
    radius: f64,
    sensitivity: f32,
}

fn grains() -> Vec<Grain> {
    let mut rng = SplitMix(0x6772_6169_6e33);
    (0..GRAINS)
        .map(|_| {
            let x = rng.uniform() * TILE as f64;
            let y = rng.uniform() * TILE as f64;
            let z = rng.normal();
            let radius = (RADIUS
                * exp_portable(RADIUS_SIGMA * z - 0.5 * RADIUS_SIGMA * RADIUS_SIGMA))
            .min(6.0);
            let sensitivity = rng.uniform() as f32;
            Grain {
                x,
                y,
                radius,
                sensitivity,
            }
        })
        .collect()
}

/// Covered share per level and texel (`[level][y * TILE + x]`), before the
/// optical blur.
fn coverage(grains: &[Grain]) -> Vec<Vec<f32>> {
    let bands = TILE / BAND;
    // Grains by the bands their disks reach (with the tile's wrap-around).
    let mut by_band: Vec<Vec<u32>> = vec![Vec::new(); bands];
    for (i, g) in grains.iter().enumerate() {
        let lo = (g.y - g.radius).floor() as i64;
        let hi = (g.y + g.radius).ceil() as i64;
        let first = lo.div_euclid(BAND as i64);
        let last = hi.div_euclid(BAND as i64);
        for b in first..=last {
            let b = b.rem_euclid(bands as i64) as usize;
            if by_band[b].last() != Some(&(i as u32)) {
                by_band[b].push(i as u32);
            }
        }
    }
    let side = TILE * SUPERSAMPLE;
    let rows = BAND * SUPERSAMPLE;
    let thresholds = THRESHOLDS.map(|t| t as f32);
    let per_band: Vec<Vec<[u8; LEVELS]>> = by_band
        .par_iter()
        .enumerate()
        .map(|(b, list)| {
            // Lowest sensitivity of the grains covering each sub-sample.
            let mut tmin = vec![2.0f32; rows * side];
            let y_base = (b * rows) as f64;
            for &i in list {
                let g = &grains[i as usize];
                let cx = g.x * SUPERSAMPLE as f64;
                let r = g.radius * SUPERSAMPLE as f64;
                let r2 = r * r;
                for shift in [-(side as f64), 0.0, side as f64] {
                    let cy = g.y * SUPERSAMPLE as f64 + shift - y_base;
                    let y0 = ((cy - r).floor().max(0.0)) as usize;
                    let y1 = ((cy + r).ceil().min(rows as f64 - 1.0)).max(-1.0);
                    if y1 < 0.0 || y0 >= rows {
                        continue;
                    }
                    for y in y0..=y1 as usize {
                        let dy = y as f64 + 0.5 - cy;
                        let rest = r2 - dy * dy;
                        if rest < 0.0 {
                            continue;
                        }
                        let x0 = (cx - r).floor() as i64;
                        let x1 = (cx + r).ceil() as i64;
                        let row = &mut tmin[y * side..(y + 1) * side];
                        for x in x0..=x1 {
                            let dx = x as f64 + 0.5 - cx;
                            if dx * dx <= rest {
                                let cell = &mut row[x.rem_euclid(side as i64) as usize];
                                if g.sensitivity < *cell {
                                    *cell = g.sensitivity;
                                }
                            }
                        }
                    }
                }
            }
            // Developed sub-samples per texel and level.
            let mut counts = vec![[0u8; LEVELS]; BAND * TILE];
            for ty in 0..BAND {
                for tx in 0..TILE {
                    let c = &mut counts[ty * TILE + tx];
                    for sy in 0..SUPERSAMPLE {
                        let row = (ty * SUPERSAMPLE + sy) * side + tx * SUPERSAMPLE;
                        for &t in &tmin[row..row + SUPERSAMPLE] {
                            for (k, threshold) in thresholds.iter().enumerate() {
                                c[k] += u8::from(t < *threshold);
                            }
                        }
                    }
                }
            }
            counts
        })
        .collect();
    let scale = 1.0 / (SUPERSAMPLE * SUPERSAMPLE) as f32;
    (0..LEVELS)
        .map(|k| {
            per_band
                .iter()
                .flat_map(|band| band.iter().map(move |c| c[k] as f32 * scale))
                .collect()
        })
        .collect()
}

/// The optical blur: a [1 4 6 4 1] / 16 binomial filter along both axes
/// (standard deviation one texel), wrapping around the tile.
fn blur(level: &[f32]) -> Vec<f32> {
    let w = [1.0f32, 4.0, 6.0, 4.0, 1.0];
    let n = TILE;
    let mut tmp = vec![0.0f32; n * n];
    for y in 0..n {
        for x in 0..n {
            let mut s = 0.0;
            for (i, wi) in w.iter().enumerate() {
                s += wi * level[y * n + (x + n + i - 2) % n];
            }
            tmp[y * n + x] = s / 16.0;
        }
    }
    let mut out = vec![0.0f32; n * n];
    for y in 0..n {
        for x in 0..n {
            let mut s = 0.0;
            for (i, wi) in w.iter().enumerate() {
                s += wi * tmp[((y + n + i - 2) % n) * n + x];
            }
            out[y * n + x] = s / 16.0;
        }
    }
    out
}

/// IEEE half-precision bits of `v` (round to nearest even; the values here
/// are small, so no infinities or NaNs).
fn f16_bits(v: f32) -> u32 {
    let bits = v.to_bits();
    let sign = (bits >> 16) & 0x8000;
    let exp = ((bits >> 23) & 0xff) as i32 - 127 + 15;
    let mant = bits & 0x7f_ffff;
    if exp <= 0 {
        if exp < -10 {
            return sign;
        }
        let m = mant | 0x80_0000;
        let shift = (14 - exp) as u32;
        let half = m >> shift;
        let rest = m & ((1 << shift) - 1);
        let mid = 1 << (shift - 1);
        let round = u32::from(rest > mid || (rest == mid && half & 1 == 1));
        return sign | (half + round);
    }
    let half = ((exp as u32) << 10) | (mant >> 13);
    let rest = mant & 0x1fff;
    let round = u32::from(rest > 0x1000 || (rest == 0x1000 && half & 1 == 1));
    sign | (half + round)
}

/// Mip sizes of the tile, finest first, down to 1 × 1.
pub fn mip_sides() -> Vec<usize> {
    std::iter::successors(Some(TILE), |&s| (s > 1).then_some(s / 2)).collect()
}

/// The tile for the shader's `film_grain_tile` buffer: for each mip (finest
/// first) and texel (row by row), LEVELS / 2 words, each two levels' grain
/// fluctuation (covered share minus the level's mean) as half floats, the
/// lower level in the low half (WGSL `unpack2x16float`).
pub fn grain_tile() -> Vec<u32> {
    let grains = grains();
    let levels: Vec<Vec<f32>> = coverage(&grains)
        .par_iter()
        .map(|level| {
            let mut b = blur(level);
            let mean = (b.iter().map(|&v| v as f64).sum::<f64>() / b.len() as f64) as f32;
            for v in &mut b {
                *v -= mean;
            }
            b
        })
        .collect();
    let mut mips = vec![levels];
    for side in mip_sides().into_iter().skip(1) {
        let prev = mips.last().unwrap();
        let next = prev
            .iter()
            .map(|level| {
                let p = side * 2;
                (0..side * side)
                    .map(|i| {
                        let (y, x) = (i / side * 2, i % side * 2);
                        (level[y * p + x]
                            + level[y * p + x + 1]
                            + level[(y + 1) * p + x]
                            + level[(y + 1) * p + x + 1])
                            * 0.25
                    })
                    .collect()
            })
            .collect();
        mips.push(next);
    }
    let mut out = Vec::new();
    for mip in &mips {
        for t in 0..mip[0].len() {
            out.extend(
                mip.as_chunks::<2>()
                    .0
                    .iter()
                    .map(|pair| f16_bits(pair[0][t]) | (f16_bits(pair[1][t]) << 16)),
            );
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn level_values(tile: &[u32], level: usize, texels: std::ops::Range<usize>) -> Vec<f32> {
        texels
            .map(|t| {
                let word = tile[t * (LEVELS / 2) + level / 2];
                let bits = if level.is_multiple_of(2) {
                    word & 0xffff
                } else {
                    word >> 16
                };
                half_to_f32(bits)
            })
            .collect()
    }

    fn half_to_f32(h: u32) -> f32 {
        let sign = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
        let exp = (h >> 10) & 0x1f;
        let mant = (h & 0x3ff) as f32;
        if exp == 0 {
            sign * mant * 2f32.powi(-24)
        } else {
            sign * (1.0 + mant / 1024.0) * 2f32.powi(exp as i32 - 15)
        }
    }

    #[test]
    fn half_floats_round_like_ieee() {
        for v in [0.0f32, 0.5, -0.25, 0.1, -0.333, 1e-5, 0.9999] {
            assert!(
                (half_to_f32(f16_bits(v)) - v).abs() <= v.abs() * 1e-3 + 1e-7,
                "{v}"
            );
        }
    }

    #[test]
    fn the_portable_exp_is_accurate() {
        for y in [-3.5, -1.0, -0.125, 0.0, 0.7, 2.9] {
            let e: f64 = f64::exp(y);
            assert!((exp_portable(y) - e).abs() < e * 1e-9, "{y}");
        }
    }

    /// The tile is the same on every platform: a checksum over its words
    /// (FNV-1a), recorded on macOS; Linux CI checks it.
    #[test]
    fn the_tile_is_the_same_everywhere_and_covers_as_expected() {
        let tile = grain_tile();
        let sides = mip_sides();
        let words: usize = sides.iter().map(|s| s * s * LEVELS / 2).sum();
        assert_eq!(tile.len(), words);
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        for w in &tile {
            hash = (hash ^ u64::from(*w)).wrapping_mul(0x100_0000_01b3);
        }
        let texels = TILE * TILE;
        let mut previous = 0.0f64;
        for level in 0..LEVELS {
            let v = level_values(&tile, level, 0..texels);
            let mean = v.iter().map(|&x| x as f64).sum::<f64>() / texels as f64;
            let sd =
                (v.iter().map(|&x| (x as f64 - mean).powi(2)).sum::<f64>() / texels as f64).sqrt();
            assert!(mean.abs() < 1e-3, "level {level} mean {mean}");
            // Boolean model: the fluctuation grows from the lightest level on.
            if level < 4 {
                assert!(sd > previous, "level {level} sd {sd}");
            }
            previous = sd;
        }
        // Coarser mips average the grain out.
        let first = level_values(&tile, 5, 0..texels);
        let second = level_values(
            &tile,
            5,
            texels + texels / 4..texels + texels / 4 + texels / 16,
        );
        let sd = |v: &[f32]| {
            (v.iter().map(|&x| (x as f64).powi(2)).sum::<f64>() / v.len() as f64).sqrt()
        };
        assert!(
            sd(&second) < sd(&first) * 0.85,
            "{} {}",
            sd(&second),
            sd(&first)
        );
        assert_eq!(hash, 0x2327_dbcd_18eb_5688, "tile checksum {hash:#x}");
    }
}
