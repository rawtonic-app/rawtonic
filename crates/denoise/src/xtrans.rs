// SPDX-License-Identifier: AGPL-3.0-or-later
//! Running the X-Trans network on overlapping tiles of the mosaic.
//!
//! Every X-Trans sensor has the same 6 × 6 layout up to a shift; the network
//! was trained on one phase of it (`CANONICAL`, the X-T1's), so each tile
//! starts on a sample at that phase: tile origins lie a multiple of 6 from
//! it, and tile edge and halo are multiples of 6. Samples beyond the mosaic
//! are filled by mirroring whole 6-sample blocks (`mirror6`), which keeps
//! each filled sample's colour; a plain mirror would break the pattern at
//! the borders. The network takes `[2, T, T]` (the samples, then each
//! sample's noise standard deviation) and returns `[1, T, T]`.

use anyhow::{Context, Result, ensure};

use crate::Backend;
use crate::noise::NoiseProfile;

/// The X-Trans layout the network was trained on (0 red, 1 green, 2 blue),
/// rows top to bottom: RawNIND's X-T1 files at their first visible sample.
pub const CANONICAL: [[u8; 6]; 6] = [
    [0, 2, 1, 2, 0, 1],
    [1, 1, 0, 1, 1, 2],
    [1, 1, 2, 1, 1, 0],
    [2, 0, 1, 0, 2, 1],
    [1, 1, 2, 1, 1, 0],
    [1, 1, 0, 1, 1, 2],
];

/// The sample (row, column) of a mosaic whose colours are `pattern` (the
/// 6 × 6 block at its origin) that sits at `CANONICAL`'s origin, or `None`
/// when `pattern` is no shift of the X-Trans layout.
pub fn phase(pattern: &[[u8; 6]; 6]) -> Option<(usize, usize)> {
    (0..36).map(|i| (i / 6, i % 6)).find(|&(dy, dx)| {
        (0..6).all(|y| (0..6).all(|x| pattern[(y + dy) % 6][(x + dx) % 6] == CANONICAL[y][x]))
    })
}

/// Index `i` (any, also far outside) mapped into `0..n` by mirroring whole
/// 6-sample blocks at both ends: the result is `i` plus a multiple of 6, so
/// it has `i`'s colour. Needs `n >= 6`.
pub fn mirror6(i: isize, n: usize) -> usize {
    assert!(n >= 6, "block mirroring needs 6 samples, has {n}");
    let n = n as isize;
    let mut i = i;
    // Each step reflects about one end; the distance to the range shrinks
    // by about `n` per two steps.
    loop {
        if i < 0 {
            i = 6 * (-1 - i.div_euclid(6)) + i.rem_euclid(6);
        } else if i >= n {
            let j = i - n;
            i = n - 6 * (j.div_euclid(6) + 1) + j.rem_euclid(6);
        } else {
            return i as usize;
        }
    }
}

/// The first row or column of the tile grid along an axis whose canonical
/// origin is at `offset` (0 … 5): at or before the mosaic's first sample, a
/// multiple of 6 from `offset`.
fn grid_start(offset: usize) -> isize {
    if offset == 0 { 0 } else { offset as isize - 6 }
}

/// The network over all tiles of `mosaic` (`width` × `height`, colours
/// `pattern` at its origin); returns the network's samples. `progress`
/// hears `(tiles done, tiles)` after each tile.
#[allow(clippy::too_many_arguments)]
pub fn run(
    mosaic: &[f32],
    width: usize,
    height: usize,
    pattern: &[[u8; 6]; 6],
    noise: &NoiseProfile,
    backend: &dyn Backend,
    noise_scale: f32,
    progress: &(dyn Fn(usize, usize) + Sync),
) -> Result<Vec<f32>> {
    let (tile, halo) = (backend.tile(), backend.halo());
    ensure!(
        tile > 2 * halo && tile % 6 == 0 && halo % 6 == 0,
        "X-Trans tiles of {tile} px with a {halo} px halo: both must be multiples of 6"
    );
    ensure!(
        width >= 6 && height >= 6 && mosaic.len() == width * height,
        "X-Trans mosaic of {width}x{height}"
    );
    let (oy, ox) = phase(pattern).context("not an X-Trans layout")?;
    let core = tile - 2 * halo;
    // One profile for every colour (`noise::estimate_xtrans`).
    let (shot, read) = noise.channels[0];
    let std: Vec<f32> = mosaic
        .iter()
        .map(|&x| (shot * x + read).max(0.0).sqrt() * noise_scale)
        .collect();
    let (y0, x0) = (grid_start(oy), grid_start(ox));
    let rows: Vec<isize> = (y0..height as isize).step_by(core).collect();
    let cols: Vec<isize> = (x0..width as isize).step_by(core).collect();
    let tiles = rows.len() * cols.len();
    let mut tiles_done = 0;
    let mut out = vec![0.0; width * height];
    let mut input = vec![0.0f32; 2 * tile * tile];
    for &ty in &rows {
        for &tx in &cols {
            let columns: Vec<usize> = (0..tile)
                .map(|x| mirror6(tx + x as isize - halo as isize, width))
                .collect();
            for (c, source) in [mosaic, &std[..]].into_iter().enumerate() {
                for y in 0..tile {
                    let sy = mirror6(ty + y as isize - halo as isize, height);
                    let row = &source[sy * width..(sy + 1) * width];
                    let dst = &mut input[(c * tile + y) * tile..(c * tile + y + 1) * tile];
                    for (d, &sx) in dst.iter_mut().zip(&columns) {
                        *d = row[sx];
                    }
                }
            }
            let result = backend.run(&input)?;
            ensure!(
                result.len() == tile * tile,
                "network returned {} values",
                result.len()
            );
            // An error, never a NaN tile in the photo or the cache.
            rawtonic_onnx::ensure_finite(&result).context("AI Denoise tile")?;
            // The core's part inside the mosaic.
            let (top, left) = (ty.max(0) as usize, tx.max(0) as usize);
            let bottom = ((ty + core as isize) as usize).min(height);
            let right = ((tx + core as isize) as usize).min(width);
            for y in top..bottom {
                let ry = (y as isize - ty) as usize + halo;
                let rx = (left as isize - tx) as usize + halo;
                out[y * width + left..y * width + right]
                    .copy_from_slice(&result[ry * tile + rx..][..right - left]);
            }
            tiles_done += 1;
            progress(tiles_done, tiles);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rotated(dy: usize, dx: usize) -> [[u8; 6]; 6] {
        let mut p = [[0; 6]; 6];
        for (y, row) in p.iter_mut().enumerate() {
            for (x, c) in row.iter_mut().enumerate() {
                // The mosaic's sample (dy, dx) is the canonical origin.
                *c = CANONICAL[(y + 6 - dy) % 6][(x + 6 - dx) % 6];
            }
        }
        p
    }

    #[test]
    fn every_shift_of_the_layout_is_found_and_nothing_else() {
        for dy in 0..6 {
            for dx in 0..6 {
                // The layout repeats under some shifts ((3, 0) and (0, 3)
                // give one pattern), so any shift that gives it will do.
                let found = phase(&rotated(dy, dx)).unwrap();
                assert_eq!(rotated(found.0, found.1), rotated(dy, dx), "{dy} {dx}");
            }
        }
        // Each dihedral image of the layout is one of its shifts.
        let mut flipped = CANONICAL;
        flipped.reverse();
        assert!(phase(&flipped).is_some());
        let mut bayerish = [[1; 6]; 6];
        for (y, row) in bayerish.iter_mut().enumerate() {
            for (x, c) in row.iter_mut().enumerate() {
                *c = [[0, 1], [1, 2]][y % 2][x % 2];
            }
        }
        assert_eq!(phase(&bayerish), None);
    }

    #[test]
    fn block_mirroring_keeps_the_colour_and_the_inside() {
        for n in [6, 7, 11, 12, 13, 50, 1009] {
            for i in -3000isize..3000 {
                let m = mirror6(i, n);
                assert!(m < n, "{i} → {m} of {n}");
                assert_eq!((m as isize - i).rem_euclid(6), 0, "{i} → {m} of {n}");
                if (0..n as isize).contains(&i) {
                    assert_eq!(m, i as usize);
                }
            }
        }
        // Whole blocks, mirrored in block order: the block before the first
        // repeats the first, the one after the last repeats the last.
        assert_eq!(
            (-6..0).map(|i| mirror6(i, 20)).collect::<Vec<_>>(),
            [0, 1, 2, 3, 4, 5]
        );
        assert_eq!(
            (-12..-6).map(|i| mirror6(i, 20)).collect::<Vec<_>>(),
            [6, 7, 8, 9, 10, 11]
        );
        assert_eq!(
            (20..26).map(|i| mirror6(i, 20)).collect::<Vec<_>>(),
            [14, 15, 16, 17, 18, 19]
        );
    }

    #[test]
    fn the_grid_starts_on_the_canonical_phase_at_or_before_the_mosaic() {
        assert_eq!(grid_start(0), 0);
        for offset in 1..6 {
            let start = grid_start(offset);
            assert!((-5..=0).contains(&start), "{offset}: {start}");
            assert_eq!((start - offset as isize).rem_euclid(6), 0);
        }
    }

    /// Records each tile's first sample's canonical position and returns
    /// the tile's first plane: an identity network.
    struct Recorder {
        tile: usize,
        halo: usize,
        canonical: Vec<[[u8; 6]; 6]>,
        colours: std::sync::Mutex<Vec<bool>>,
    }

    impl Backend for Recorder {
        fn run(&self, input: &[f32]) -> Result<Vec<f32>> {
            let t = self.tile;
            assert_eq!(input.len(), 2 * t * t, "two planes of a tile");
            // The test mosaic encodes each sample's colour in its value.
            let ok = (0..t).all(|y| {
                (0..t).all(|x| {
                    let colour = (input[y * t + x] * 10.0).round() as u8 % 3;
                    colour == self.canonical[0][y % 6][x % 6]
                })
            });
            self.colours.lock().unwrap().push(ok);
            Ok(input[..t * t].to_vec())
        }

        fn tile(&self) -> usize {
            self.tile
        }

        fn halo(&self) -> usize {
            self.halo
        }
    }

    fn colour_coded(width: usize, height: usize, pattern: &[[u8; 6]; 6]) -> Vec<f32> {
        (0..width * height)
            .map(|i| {
                let (y, x) = (i / width, i % width);
                pattern[y % 6][x % 6] as f32 / 10.0 + 0.3 + (i % 7) as f32 * 1e-3
            })
            .collect()
    }

    #[test]
    fn every_tile_starts_at_the_canonical_phase_and_the_cores_cover_the_mosaic() {
        let profile = NoiseProfile {
            channels: [(1e-3, 1e-5); 4],
        };
        for (dy, dx) in [(0, 0), (1, 4), (5, 5), (3, 0)] {
            let pattern = rotated(dy, dx);
            // Neither side a multiple of 6 or of the core.
            let (w, h) = (205, 131);
            let mosaic = colour_coded(w, h, &pattern);
            let backend = Recorder {
                tile: 96,
                halo: 12,
                canonical: vec![CANONICAL],
                colours: Default::default(),
            };
            let heard = std::sync::Mutex::new(Vec::new());
            let out = run(&mosaic, w, h, &pattern, &profile, &backend, 1.0, &|d, t| {
                heard.lock().unwrap().push((d, t))
            })
            .unwrap();
            assert_eq!(out, mosaic, "an identity network returns the mosaic");
            let colours = backend.colours.into_inner().unwrap();
            assert!(colours.iter().all(|&ok| ok), "{dy} {dx}: {colours:?}");
            // 72 px cores from at most 5 samples before the origin.
            let tiles = (h + 5).div_ceil(72) * (w + 5).div_ceil(72);
            assert!(colours.len() <= tiles && colours.len() >= h.div_ceil(72) * w.div_ceil(72));
            let heard = heard.into_inner().unwrap();
            assert_eq!(heard.last(), Some(&(colours.len(), colours.len())));
        }
    }

    #[test]
    fn the_noise_plane_is_the_profile_and_a_tile_edge_off_the_grid_is_refused() {
        struct Sigma;
        impl Backend for Sigma {
            fn run(&self, input: &[f32]) -> Result<Vec<f32>> {
                Ok(input[48 * 48..].to_vec())
            }
            fn tile(&self) -> usize {
                48
            }
            fn halo(&self) -> usize {
                6
            }
        }
        let pattern = CANONICAL;
        let (w, h) = (60, 40);
        let mosaic = colour_coded(w, h, &pattern);
        let (shot, read) = (2e-3f32, 3e-5f32);
        let profile = NoiseProfile {
            channels: [(shot, read); 4],
        };
        let out = run(&mosaic, w, h, &pattern, &profile, &Sigma, 1.5, &|_, _| {}).unwrap();
        for (s, x) in out.iter().zip(&mosaic) {
            assert_eq!(*s, (shot * x + read).max(0.0).sqrt() * 1.5);
        }
        struct Odd;
        impl Backend for Odd {
            fn run(&self, _: &[f32]) -> Result<Vec<f32>> {
                unreachable!()
            }
            fn tile(&self) -> usize {
                50
            }
            fn halo(&self) -> usize {
                5
            }
        }
        assert!(run(&mosaic, w, h, &pattern, &profile, &Odd, 1.0, &|_, _| {}).is_err());
        let flat = vec![0.2; w * h];
        let not_xtrans = [[1u8; 6]; 6];
        assert!(run(&flat, w, h, &not_xtrans, &profile, &Sigma, 1.0, &|_, _| {}).is_err());
    }
}
