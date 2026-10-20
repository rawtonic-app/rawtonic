// SPDX-License-Identifier: AGPL-3.0-or-later
//! Packing a Bayer mosaic into R, G, B, G planes and running the network on
//! overlapping tiles.

use anyhow::{Context, Result, ensure};

use crate::Backend;
use crate::noise::NoiseProfile;

/// Tile edge in packed pixels, as the network was exported.
pub const TILE: usize = 320;
/// Margin around the kept core of each tile.
pub const HALO: usize = 40;

/// Four planes of `width / 2` × `height / 2`, plane-major.
pub struct Planes {
    pub width: usize,
    pub height: usize,
    pub data: Vec<f32>,
}

impl Planes {
    pub fn plane(&self, c: usize) -> &[f32] {
        let n = self.width * self.height;
        &self.data[c * n..(c + 1) * n]
    }
}

/// Offsets (row, column) in the 2 × 2 tile of R, the green beside R, B and
/// the green beside B — the network's plane order.
fn order(pattern: [u8; 4]) -> [(usize, usize); 4] {
    let at = |colour: u8| {
        let i = pattern.iter().position(|&c| c == colour).unwrap_or(0);
        (i / 2, i % 2)
    };
    let (ry, rx) = at(0);
    let (by, bx) = at(2);
    [(ry, rx), (ry, bx), (by, bx), (by, rx)]
}

pub fn pack(mosaic: &[f32], width: usize, height: usize, pattern: [u8; 4]) -> Planes {
    let (pw, ph) = (width / 2, height / 2);
    let mut data = vec![0.0; 4 * pw * ph];
    for (c, (dy, dx)) in order(pattern).into_iter().enumerate() {
        let plane = &mut data[c * pw * ph..(c + 1) * pw * ph];
        for y in 0..ph {
            for x in 0..pw {
                plane[y * pw + x] = mosaic[(2 * y + dy) * width + 2 * x + dx];
            }
        }
    }
    Planes {
        width: pw,
        height: ph,
        data,
    }
}

pub fn unpack(planes: &Planes, width: usize, height: usize, pattern: [u8; 4]) -> Vec<f32> {
    let mut mosaic = vec![0.0; width * height];
    let (pw, ph) = (planes.width, planes.height);
    for (c, (dy, dx)) in order(pattern).into_iter().enumerate() {
        let plane = planes.plane(c);
        for y in 0..ph {
            for x in 0..pw {
                mosaic[(2 * y + dy) * width + 2 * x + dx] = plane[y * pw + x];
            }
        }
    }
    mosaic
}

/// Mirror index without repeating the edge (numpy's "reflect").
fn reflect(i: isize, n: usize) -> usize {
    let n = n as isize;
    if n == 1 {
        return 0;
    }
    let period = 2 * (n - 1);
    let mut i = i.rem_euclid(period);
    if i >= n {
        i = period - i;
    }
    i as usize
}

/// The network over all tiles; returns denoised planes.
pub fn run(
    planes: &Planes,
    noise: &NoiseProfile,
    backend: &dyn Backend,
    noise_scale: f32,
) -> Result<Planes> {
    let (pw, ph) = (planes.width, planes.height);
    let tile = backend.tile();
    ensure!(tile > 2 * HALO, "tile of {tile} px");
    let core = tile - 2 * HALO;
    ensure!(pw >= 2 && ph >= 2, "mosaic too small: {pw}x{ph} planes");
    let std: Vec<Vec<f32>> = (0..4)
        .map(|c| {
            let (shot, read) = noise.channels[c];
            planes
                .plane(c)
                .iter()
                .map(|&x| (shot * x + read).max(0.0).sqrt() * noise_scale)
                .collect()
        })
        .collect();
    let mut out = vec![0.0; 4 * pw * ph];
    let mut input = vec![0.0f32; 8 * tile * tile];
    for ty in (0..ph).step_by(core) {
        for tx in (0..pw).step_by(core) {
            for c in 0..8 {
                let source = if c < 4 {
                    planes.plane(c)
                } else {
                    &std[c - 4][..]
                };
                for y in 0..tile {
                    let sy = reflect(ty as isize + y as isize - HALO as isize, ph);
                    let row = &source[sy * pw..(sy + 1) * pw];
                    let dst = &mut input[(c * tile + y) * tile..(c * tile + y + 1) * tile];
                    for (x, d) in dst.iter_mut().enumerate() {
                        *d = row[reflect(tx as isize + x as isize - HALO as isize, pw)];
                    }
                }
            }
            let result = backend.run(&input)?;
            ensure!(
                result.len() == 4 * tile * tile,
                "network returned {} values",
                result.len()
            );
            // A GPU fault can return NaN without an error (Core ML): an
            // error, never a NaN tile in the photo or the cache.
            rawtonic_onnx::ensure_finite(&result).context("AI Denoise tile")?;
            let (hh, ww) = (core.min(ph - ty), core.min(pw - tx));
            for c in 0..4 {
                for y in 0..hh {
                    let src = &result[(c * tile + HALO + y) * tile + HALO..][..ww];
                    out[c * pw * ph + (ty + y) * pw + tx..][..ww].copy_from_slice(src);
                }
            }
        }
    }
    Ok(Planes {
        width: pw,
        height: ph,
        data: out,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reflection_matches_numpy() {
        // np.pad([0, 1, 2, 3], 3, mode="reflect") = 3 2 1 | 0 1 2 3 | 2 1 0
        let got: Vec<usize> = (-3..7).map(|i| reflect(i, 4)).collect();
        assert_eq!(got, [3, 2, 1, 0, 1, 2, 3, 2, 1, 0]);
    }

    #[test]
    fn packing_round_trips_and_orders_the_planes() {
        let (w, h) = (4, 2);
        let mosaic: Vec<f32> = (0..8).map(|v| v as f32).collect();
        // GRBG: R at (0, 1), B at (1, 0).
        let planes = pack(&mosaic, w, h, [1, 0, 2, 1]);
        assert_eq!(planes.plane(0), &[1.0, 3.0]); // R
        assert_eq!(planes.plane(1), &[0.0, 2.0]); // G beside R
        assert_eq!(planes.plane(2), &[4.0, 6.0]); // B
        assert_eq!(planes.plane(3), &[5.0, 7.0]); // G beside B
        assert_eq!(unpack(&planes, w, h, [1, 0, 2, 1]), mosaic);
    }
}
