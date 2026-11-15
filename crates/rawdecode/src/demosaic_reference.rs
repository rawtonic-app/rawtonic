//! The Bayer kernel as it was before its speed-up (2026-11-15), kept
//! verbatim as the reference the faster `bayer_tile` must match bit for bit
//! (tests in `demosaic`). Only the name and the `super::` imports differ.

use super::{EPS, EPS_SQ, Tile};

pub(super) fn bayer_tile(tile: &Tile) -> Vec<[f32; 3]> {
    let w = tile.w;
    let h = tile.h;
    let cfa = &tile.data;
    let col = &tile.colors;
    let n = w * h;
    let (w1, w2, w3, w4) = (w, 2 * w, 3 * w, 4 * w);

    let mut rgb = vec![[0.0f32; 3]; n];
    for i in 0..n {
        rgb[i][col[i] as usize] = cfa[i];
    }

    // Direction: squared high-pass along each axis. The kernel
    // [1, -3, -1, 6, -1, -3, 1] sums to zero on both colours of a Bayer
    // line, so flat colour areas give zero whatever their hue.
    let mut hp_v = vec![0.0f32; n];
    let mut hp_h = vec![0.0f32; n];
    for r in 3..h - 3 {
        for c in 3..w - 3 {
            let i = r * w + c;
            let v = cfa[i - w3] - cfa[i - w1] - cfa[i + w1] + cfa[i + w3]
                - 3.0 * (cfa[i - w2] + cfa[i + w2])
                + 6.0 * cfa[i];
            let hz = cfa[i - 3] - cfa[i - 1] - cfa[i + 1] + cfa[i + 3]
                - 3.0 * (cfa[i - 2] + cfa[i + 2])
                + 6.0 * cfa[i];
            hp_v[i] = v * v;
            hp_h[i] = hz * hz;
        }
    }
    let mut vh = vec![0.5f32; n];
    for r in 4..h - 4 {
        for c in 4..w - 4 {
            let i = r * w + c;
            let v = EPS_SQ + hp_v[i - w1] + hp_v[i] + hp_v[i + w1];
            let hz = EPS_SQ + hp_h[i - 1] + hp_h[i] + hp_h[i + 1];
            vh[i] = v / (v + hz);
        }
    }

    // Green at red and blue sites.
    for r in 5..h - 5 {
        for c in 5..w - 5 {
            let i = r * w + c;
            if col[i] == 1 {
                continue;
            }
            let ci = cfa[i];
            let n_grad = EPS
                + (cfa[i - w1] - cfa[i + w1]).abs()
                + (ci - cfa[i - w2]).abs()
                + (cfa[i - w1] - cfa[i - w3]).abs()
                + (cfa[i - w2] - cfa[i - w4]).abs();
            let s_grad = EPS
                + (cfa[i + w1] - cfa[i - w1]).abs()
                + (ci - cfa[i + w2]).abs()
                + (cfa[i + w1] - cfa[i + w3]).abs()
                + (cfa[i + w2] - cfa[i + w4]).abs();
            let w_grad = EPS
                + (cfa[i - 1] - cfa[i + 1]).abs()
                + (ci - cfa[i - 2]).abs()
                + (cfa[i - 1] - cfa[i - 3]).abs()
                + (cfa[i - 2] - cfa[i - 4]).abs();
            let e_grad = EPS
                + (cfa[i + 1] - cfa[i - 1]).abs()
                + (ci - cfa[i + 2]).abs()
                + (cfa[i + 1] - cfa[i + 3]).abs()
                + (cfa[i + 2] - cfa[i + 4]).abs();

            // One-sided estimates: the neighbouring green corrected by half
            // the own colour's change towards it (colour differences are
            // smooth on the encoded data).
            let n_est = cfa[i - w1] + 0.5 * (ci - cfa[i - w2]);
            let s_est = cfa[i + w1] + 0.5 * (ci - cfa[i + w2]);
            let w_est = cfa[i - 1] + 0.5 * (ci - cfa[i - 2]);
            let e_est = cfa[i + 1] + 0.5 * (ci - cfa[i + 2]);

            let v_est = (s_grad * n_est + n_grad * s_est) / (n_grad + s_grad);
            let h_est = (w_grad * e_est + e_grad * w_est) / (e_grad + w_grad);

            let central = vh[i];
            let around = 0.25 * (vh[i - w1 - 1] + vh[i - w1 + 1] + vh[i + w1 - 1] + vh[i + w1 + 1]);
            let disc = if (0.5 - central).abs() < (0.5 - around).abs() {
                around
            } else {
                central
            };
            rgb[i][1] = (h_est * disc + v_est * (1.0 - disc)).max(0.0);
        }
    }

    // Diagonal direction at red/blue sites.
    let mut hp_p = vec![0.0f32; n];
    let mut hp_q = vec![0.0f32; n];
    for r in 3..h - 3 {
        for c in 3..w - 3 {
            let i = r * w + c;
            let p = cfa[i - w3 - 3] - cfa[i - w1 - 1] - cfa[i + w1 + 1] + cfa[i + w3 + 3]
                - 3.0 * (cfa[i - w2 - 2] + cfa[i + w2 + 2])
                + 6.0 * cfa[i];
            let q = cfa[i - w3 + 3] - cfa[i - w1 + 1] - cfa[i + w1 - 1] + cfa[i + w3 - 3]
                - 3.0 * (cfa[i - w2 + 2] + cfa[i + w2 - 2])
                + 6.0 * cfa[i];
            hp_p[i] = p * p;
            hp_q[i] = q * q;
        }
    }
    let mut pq = vec![0.5f32; n];
    for r in 4..h - 4 {
        for c in 4..w - 4 {
            let i = r * w + c;
            let p = EPS_SQ + hp_p[i - w1 - 1] + hp_p[i] + hp_p[i + w1 + 1];
            let q = EPS_SQ + hp_q[i - w1 + 1] + hp_q[i] + hp_q[i + w1 - 1];
            pq[i] = p / (p + q);
        }
    }

    // Red at blue sites and blue at red sites, along the diagonals.
    for r in 7..h - 7 {
        for c in 7..w - 7 {
            let i = r * w + c;
            let own = col[i] as usize;
            if own == 1 {
                continue;
            }
            let x = 2 - own;
            let g = |k: usize| rgb[k][1];
            let v = |k: usize| rgb[k][x];
            let (nw, ne, sw, se) = (i - w1 - 1, i - w1 + 1, i + w1 - 1, i + w1 + 1);
            let nw_grad = EPS
                + (v(nw) - v(se)).abs()
                + (v(nw) - v(i - w3 - 3)).abs()
                + (g(i) - g(i - w2 - 2)).abs();
            let ne_grad = EPS
                + (v(ne) - v(sw)).abs()
                + (v(ne) - v(i - w3 + 3)).abs()
                + (g(i) - g(i - w2 + 2)).abs();
            let sw_grad = EPS
                + (v(sw) - v(ne)).abs()
                + (v(sw) - v(i + w3 - 3)).abs()
                + (g(i) - g(i + w2 - 2)).abs();
            let se_grad = EPS
                + (v(se) - v(nw)).abs()
                + (v(se) - v(i + w3 + 3)).abs()
                + (g(i) - g(i + w2 + 2)).abs();
            let p_est =
                (nw_grad * (v(se) - g(se)) + se_grad * (v(nw) - g(nw))) / (nw_grad + se_grad);
            let q_est =
                (ne_grad * (v(sw) - g(sw)) + sw_grad * (v(ne) - g(ne))) / (ne_grad + sw_grad);
            let central = pq[i];
            let around = 0.25 * (pq[nw] + pq[ne] + pq[sw] + pq[se]);
            let disc = if (0.5 - central).abs() < (0.5 - around).abs() {
                around
            } else {
                central
            };
            rgb[i][x] = (g(i) + (1.0 - disc) * p_est + disc * q_est).max(0.0);
        }
    }

    // Red and blue at green sites, along the axes.
    for r in 10..h - 10 {
        for c in 10..w - 10 {
            let i = r * w + c;
            if col[i] != 1 {
                continue;
            }
            let central = vh[i];
            let around = 0.25 * (vh[i - w1 - 1] + vh[i - w1 + 1] + vh[i + w1 - 1] + vh[i + w1 + 1]);
            let disc = if (0.5 - central).abs() < (0.5 - around).abs() {
                around
            } else {
                central
            };
            let gi = rgb[i][1];
            for x in [0usize, 2] {
                let v = |k: usize| rgb[k][x];
                let g = |k: usize| rgb[k][1];
                let n_grad = EPS
                    + (gi - g(i - w2)).abs()
                    + (v(i - w1) - v(i + w1)).abs()
                    + (v(i - w1) - v(i - w3)).abs();
                let s_grad = EPS
                    + (gi - g(i + w2)).abs()
                    + (v(i + w1) - v(i - w1)).abs()
                    + (v(i + w1) - v(i + w3)).abs();
                let w_grad = EPS
                    + (gi - g(i - 2)).abs()
                    + (v(i - 1) - v(i + 1)).abs()
                    + (v(i - 1) - v(i - 3)).abs();
                let e_grad = EPS
                    + (gi - g(i + 2)).abs()
                    + (v(i + 1) - v(i - 1)).abs()
                    + (v(i + 1) - v(i + 3)).abs();
                let n_est = v(i - w1) - g(i - w1);
                let s_est = v(i + w1) - g(i + w1);
                let w_est = v(i - 1) - g(i - 1);
                let e_est = v(i + 1) - g(i + 1);
                let v_est = (n_grad * s_est + s_grad * n_est) / (n_grad + s_grad);
                let h_est = (e_grad * w_est + w_grad * e_est) / (e_grad + w_grad);
                rgb[i][x] = (gi + (1.0 - disc) * v_est + disc * h_est).max(0.0);
            }
        }
    }
    rgb
}
