//! The lateral CA estimate as it was before its speed-up (2026-10-26),
//! kept verbatim as the reference the faster functions must match bit for
//! bit (tests in `lateral_ca`). Only names and the `super::` imports differ.

use super::*;

pub(super) fn downscale(image: &Rgb32FImage, long_edge: u32) -> Rgb32FImage {
    let (w, h) = image.dimensions();
    let factor = (w.max(h) as f32 / long_edge as f32).floor().max(1.0) as u32;
    if factor == 1 {
        return image.clone();
    }
    let (sw, sh) = (w / factor, h / factor);
    let mut out = Rgb32FImage::new(sw, sh);
    out.enumerate_rows_mut().par_bridge().for_each(|(_, row)| {
        for (x, y, p) in row {
            let mut acc = [0.0f32; 3];
            for j in 0..factor {
                for i in 0..factor {
                    let q = image.get_pixel(x * factor + i, y * factor + j);
                    for c in 0..3 {
                        acc[c] += q[c];
                    }
                }
            }
            *p = image::Rgb(acc.map(|v| v / (factor * factor) as f32));
        }
    });
    out
}
/// The red and blue shift (pixels along the outward radius) that is left
/// after `model` at each of `centres` of `image`, measured at the image's
/// own resolution on square-root values. With `check_edges` a patch is
/// kept only when green's edges run across the radius.
pub(super) fn measure_patches(
    image: &Rgb32FImage,
    centres: &[(f32, f32)],
    model: CaModel,
    check_edges: bool,
) -> Vec<Patch> {
    let (w, h) = image.dimensions();
    let (cx, cy) = ((w as f32 - 1.0) / 2.0, (h as f32 - 1.0) / 2.0);
    let inv_hd2 = 1.0 / (cx * cx + cy * cy);
    let raw = image.as_raw();
    centres
        .par_iter()
        .filter_map(|&(px, py)| {
            let (dx, dy) = (px - cx, py - cy);
            let radius = (dx * dx + dy * dy).sqrt();
            if radius < 1.0 {
                return None;
            }
            let (ux, uy) = (dx / radius, dy / radius);
            // A local square-root copy of the three planes around the patch,
            // wide enough for the model's displacement and the search.
            let reach = |m: (f32, f32)| {
                let corner = (HALF as f32 + 1.0) * std::f32::consts::SQRT_2;
                let far = radius + corner;
                (m.0.abs() + m.1.abs() * far * far * inv_hd2) * far
            };
            let margin =
                (HALF as f32 + MAX_SHIFT + 3.0 + reach(model.red).max(reach(model.blue))).ceil();
            let x0 = (px - margin).floor().max(0.0) as i64;
            let y0 = (py - margin).floor().max(0.0) as i64;
            let x1 = ((px + margin).ceil() as i64).min(w as i64 - 1);
            let y1 = ((py + margin).ceil() as i64).min(h as i64 - 1);
            if x1 - x0 < 2 || y1 - y0 < 2 {
                return None;
            }
            let (lw, lh) = ((x1 - x0 + 1) as usize, (y1 - y0 + 1) as usize);
            let mut local = [
                vec![0.0f32; lw * lh],
                vec![0.0f32; lw * lh],
                vec![0.0f32; lw * lh],
            ];
            for j in 0..lh {
                let row = (y0 as usize + j) * w as usize;
                for i in 0..lw {
                    let p = (row + x0 as usize + i) * 3;
                    for (c, plane) in local.iter_mut().enumerate() {
                        plane[j * lw + i] = raw[p + c].max(0.0).sqrt();
                    }
                }
            }
            let at = |plane: &[f32], x: f32, y: f32| -> f32 {
                let x = (x - x0 as f32).clamp(0.0, lw as f32 - 1.001);
                let y = (y - y0 as f32).clamp(0.0, lh as f32 - 1.001);
                let (xi, yi) = (x.floor() as usize, y.floor() as usize);
                let (fx, fy) = (x - xi as f32, y - yi as f32);
                let i = yi * lw + xi;
                (plane[i] * (1.0 - fx) + plane[i + 1] * fx) * (1.0 - fy)
                    + (plane[i + lw] * (1.0 - fx) + plane[i + lw + 1] * fx) * fy
            };
            let g = &local[1];
            let mut along = 0.0f32;
            let mut across = 0.0f32;
            for j in -HALF..=HALF {
                for i in -HALF..=HALF {
                    let (x, y) = (px + i as f32, py + j as f32);
                    let gx = at(g, x + 1.0, y) - at(g, x - 1.0, y);
                    let gy = at(g, x, y + 1.0) - at(g, x, y - 1.0);
                    let radial = gx * ux + gy * uy;
                    let tangential = -gx * uy + gy * ux;
                    along += radial * radial;
                    across += tangential * tangential;
                }
            }
            let cells = ((2 * HALF + 1) * (2 * HALF + 1)) as f32;
            if check_edges && (along / cells < 2e-4 || along < 2.0 * across) {
                return None;
            }
            let n = (2 * HALF + 1) as usize;
            let mut refs = Vec::with_capacity(n * n);
            for j in -HALF..=HALF {
                for i in -HALF..=HALF {
                    refs.push(at(g, px + i as f32, py + j as f32));
                }
            }
            if !normalise(&mut refs) {
                return None;
            }
            let shift = |plane: &[f32], m: (f32, f32)| -> Option<f32> {
                let score = |s: f32| -> f32 {
                    let mut v = Vec::with_capacity(n * n);
                    for j in -HALF..=HALF {
                        for i in -HALF..=HALF {
                            let (x, y) = (px + i as f32 + s * ux, py + j as f32 + s * uy);
                            let (x, y) = modelled(m, x, y, cx, cy, inv_hd2);
                            v.push(at(plane, x, y));
                        }
                    }
                    if !normalise(&mut v) {
                        return f32::MAX;
                    }
                    v.iter().zip(&refs).map(|(a, b)| (a - b) * (a - b)).sum()
                };
                parabolic_minimum(&score)
            };
            let red = shift(&local[0], model.red)?;
            let blue = shift(&local[2], model.blue)?;
            Some(Patch {
                x: px,
                y: py,
                radius,
                red,
                blue,
            })
        })
        .collect()
}
/// The colour that the local luminance does not explain at the edges in
/// windows around `centres` of `image` when red and blue are sampled with
/// `model`: in square-root RGB, `u = √R − √G` and `v = √B − √G` are fitted
/// as linear in `Y = √((R + 2G + B) / 4)` in every 5 × 5 window (as the
/// edge-fringe pass does) and the squared residual is summed over pixels
/// whose window holds an edge. Lateral CA leaves colour on one side of an
/// edge and its complement on the other, which no such fit explains;
/// a two-colour edge is explained. Sum and number of pixels.
pub(super) fn fringe_cost(
    image: &Rgb32FImage,
    centres: &[(f32, f32)],
    model: CaModel,
) -> (f64, usize) {
    let (w, h) = image.dimensions();
    let (cx, cy) = ((w as f32 - 1.0) / 2.0, (h as f32 - 1.0) / 2.0);
    let inv_hd2 = 1.0 / (cx * cx + cy * cy);
    let raw = image.as_raw();
    let side = (2 * CHECK_HALF + 1) as usize;
    centres
        .par_iter()
        .map(|&(px, py)| {
            let (px, py) = (px.round() as i64, py.round() as i64);
            if px - CHECK_HALF < 1
                || py - CHECK_HALF < 1
                || px + CHECK_HALF + 1 >= w as i64
                || py + CHECK_HALF + 1 >= h as i64
            {
                return (0.0, 0);
            }
            let value = |c: usize, x: i64, y: i64| -> f32 {
                let x = x.clamp(0, w as i64 - 1) as usize;
                let y = y.clamp(0, h as i64 - 1) as usize;
                raw[(y * w as usize + x) * 3 + c].max(0.0).sqrt()
            };
            let sample = |c: usize, x: f32, y: f32| -> f32 {
                let x = x.clamp(0.0, w as f32 - 1.001);
                let y = y.clamp(0.0, h as f32 - 1.001);
                let (xi, yi) = (x.floor() as i64, y.floor() as i64);
                let (fx, fy) = (x - xi as f32, y - yi as f32);
                (value(c, xi, yi) * (1.0 - fx) + value(c, xi + 1, yi) * fx) * (1.0 - fy)
                    + (value(c, xi, yi + 1) * (1.0 - fx) + value(c, xi + 1, yi + 1) * fx) * fy
            };
            let n = side * side;
            let (mut y_, mut u, mut v) = (vec![0.0f32; n], vec![0.0f32; n], vec![0.0f32; n]);
            for j in 0..side {
                for i in 0..side {
                    let (x, y) = (px - CHECK_HALF + i as i64, py - CHECK_HALF + j as i64);
                    let g = value(1, x, y);
                    let (rx, ry) = modelled(model.red, x as f32, y as f32, cx, cy, inv_hd2);
                    let (bx, by) = modelled(model.blue, x as f32, y as f32, cx, cy, inv_hd2);
                    let (r, b) = (sample(0, rx, ry), sample(2, bx, by));
                    let k = j * side + i;
                    y_[k] = ((r * r + 2.0 * g * g + b * b) / 4.0).sqrt();
                    u[k] = r - g;
                    v[k] = b - g;
                }
            }
            let (mut sum, mut count) = (0.0f64, 0usize);
            let r = CHECK_RADIUS as usize;
            let cells = ((2 * r + 1) * (2 * r + 1)) as f32;
            for j in r..side - r {
                for i in r..side - r {
                    let (mut sy, mut syy, mut su, mut sv, mut syu, mut syv) =
                        (0.0f32, 0.0f32, 0.0f32, 0.0f32, 0.0f32, 0.0f32);
                    for jj in j - r..=j + r {
                        for ii in i - r..=i + r {
                            let k = jj * side + ii;
                            sy += y_[k];
                            syy += y_[k] * y_[k];
                            su += u[k];
                            sv += v[k];
                            syu += y_[k] * u[k];
                            syv += y_[k] * v[k];
                        }
                    }
                    let my = sy / cells;
                    let var = (syy / cells - my * my).max(0.0);
                    if var.sqrt() < EDGE_STD {
                        continue;
                    }
                    let k = j * side + i;
                    let (mu, mv) = (su / cells, sv / cells);
                    let bu = (syu / cells - my * mu) / (var + 4e-4);
                    let bv = (syv / cells - my * mv) / (var + 4e-4);
                    let du = u[k] - (mu + bu * (y_[k] - my));
                    let dv = v[k] - (mv + bv * (y_[k] - my));
                    sum += (du * du + dv * dv) as f64;
                    count += 1;
                }
            }
            (sum, count)
        })
        .reduce(|| (0.0, 0), |a, b| (a.0 + b.0, a.1 + b.1))
}
/// Found on the reduced working copy, then refined twice at the photo's
/// full resolution on the same patches; `None` with too few usable edges.
pub(super) fn analyse(image: &Rgb32FImage, report: bool) -> Option<Analysis> {
    let (w, h) = image.dimensions();
    let small = downscale(image, WORK_EDGE);
    let (sw, sh) = small.dimensions();
    let factor = (w as f32 / sw as f32).round().max(1.0);
    let (scx, scy) = ((sw as f32 - 1.0) / 2.0, (sh as f32 - 1.0) / 2.0);
    let small_hd = (scx * scx + scy * scy).sqrt();
    let mut centres = Vec::new();
    let border = HALF as u32 + MAX_SHIFT as u32 + 2;
    let mut y = border;
    while y + border < sh {
        let mut x = border;
        while x + border < sw {
            let (dx, dy) = (x as f32 - scx, y as f32 - scy);
            if (dx * dx + dy * dy).sqrt() > 0.35 * small_hd {
                centres.push((x as f32, y as f32));
            }
            x += (HALF * 2) as u32;
        }
        y += (HALF * 2) as u32;
    }
    let first = measure_patches(&small, &centres, CaModel::NONE, true);
    if first.len() < 12 {
        return None;
    }
    let coarse = fit(&first, small_hd);
    if report {
        println!(
            "  v2 reduced 1/{factor}: {coarse:?} ({} patches)",
            first.len()
        );
    }
    // The same patches at full resolution.
    let (cx, cy) = ((w as f32 - 1.0) / 2.0, (h as f32 - 1.0) / 2.0);
    let hd = (cx * cx + cy * cy).sqrt();
    let full: Vec<(f32, f32)> = first
        .iter()
        .map(|p| ((p.x + 0.5) * factor - 0.5, (p.y + 0.5) * factor - 0.5))
        .collect();
    // Refined twice; a refinement whose second step does not settle (it
    // moves the corners by more than half as much as the first) is noise.
    let mut refined = Some(coarse);
    let mut last_step = f32::MAX;
    for pass in 0..2 {
        let Some(model) = refined else { break };
        let patches = measure_patches(image, &full, model, false);
        if patches.len() < 12 {
            refined = None;
            break;
        }
        let rest = fit(&patches, hd);
        let step = corner(rest);
        if report {
            println!(
                "  v2 refine {pass}: rest {rest:?} ({} patches)",
                patches.len()
            );
        }
        if pass > 0 && step > 0.5 * last_step && step > 5e-5 {
            refined = None;
            break;
        }
        last_step = step;
        refined = Some(add(model, rest));
    }
    Some(Analysis {
        full,
        coarse,
        refined,
        half_diagonal: hd,
    })
}
/// Version 2 of the estimate (notes: reference/lateral-ca.md):
/// - found on the reduced working copy, then refined twice at the photo's
///   full resolution, where the fringes are seen (a reduced copy measures a
///   larger CA: soft red and blue halos weigh more there);
/// - the refined and the reduced copy's model are tried at a quarter, half,
///   three quarters and full strength, and the one that leaves the least
///   colour at the measured edges at full resolution (`fringe_cost`) wins,
///   if it lowers that colour by `MIN_GAIN` at least; else `None` (the
///   photo is not corrected), so the correction never adds fringes.
pub(super) fn estimate_v2(image: &Rgb32FImage) -> Option<CaModel> {
    let report = std::env::var_os("RAWTONIC_CA_PROFILE").is_some();
    let start = std::time::Instant::now();
    let analysis = analyse(image, report)?;
    let full = &analysis.full;
    // The candidate that leaves the least colour at the edges.
    let (none, count) = fringe_cost(image, full, CaModel::NONE);
    if count == 0 || none <= 0.0 {
        return None;
    }
    let mut best = (none, CaModel::NONE);
    for (g, candidate) in candidates(&analysis) {
        let cost = fringe_cost(image, full, candidate).0;
        if report {
            println!("  v2 check {g} x {candidate:?}: {:.4}", cost / none);
        }
        if cost < best.0 {
            best = (cost, candidate);
        }
    }
    if report {
        println!(
            "  v2 choice: {:?} ({:.4}, {:?})",
            best.1,
            best.0 / none,
            start.elapsed()
        );
    }
    (best.0 <= (1.0 - MIN_GAIN) * none).then_some(best.1)
}
/// Version 3 where the lens correction has a lateral CA correction of its
/// own (`own`: the camera's data or a profile's TCA): the same check as
/// version 2 scores `own`, no correction, version 2's candidates and `own`
/// refined at full resolution (its remainder added at a quarter to full
/// strength). The best of them replaces `own` only when the evidence is
/// clear: it leaves at least `VERSUS_MARGIN` less colour at the edges than
/// `own` on all patches, on every other one of them alike and on the outer
/// ones (which must carry `MIN_OUTER_SHARE` of the cost), and the measured
/// red and blue shift it leaves is clearly smaller (`MAX_SHIFT_SHARE`).
/// `None` keeps `own` (also when the photo has too few usable edges).
pub(super) fn estimate_versus(image: &Rgb32FImage, own: CaModel) -> Option<CaModel> {
    let report = std::env::var_os("RAWTONIC_CA_PROFILE").is_some();
    let start = std::time::Instant::now();
    let analysis = analyse(image, report)?;
    let full = &analysis.full;
    let (w, h) = image.dimensions();
    let (cx, cy) = ((w as f32 - 1.0) / 2.0, (h as f32 - 1.0) / 2.0);
    // Four disjoint groups of patches: every other one (two halves), each
    // inner and outer. Their costs add up to the cost on all patches.
    let groups: [Vec<(f32, f32)>; 4] = [0, 1, 2, 3].map(|g| {
        full.iter()
            .enumerate()
            .filter(|&(i, &(x, y))| {
                let outer =
                    ((x - cx).powi(2) + (y - cy).powi(2)).sqrt() >= OUTER * analysis.half_diagonal;
                (i % 2) + 2 * usize::from(outer) == g
            })
            .map(|(_, c)| *c)
            .collect()
    });
    let cost = |model: CaModel| -> [f64; 4] {
        [0, 1, 2, 3].map(|g| fringe_cost(image, &groups[g], model).0)
    };
    // All, the two halves and the outer patches.
    let parts = |c: [f64; 4]| {
        [
            c[0] + c[1] + c[2] + c[3],
            c[0] + c[2],
            c[1] + c[3],
            c[2] + c[3],
        ]
    };
    let own_cost = parts(cost(own));
    if own_cost[1] <= 0.0 || own_cost[2] <= 0.0 {
        return None;
    }
    let own_patches = measure_patches(image, full, own, false);
    let refined_own: Vec<(f32, CaModel)> = if own_patches.len() < 12 {
        Vec::new()
    } else {
        let rest = fit(&own_patches, analysis.half_diagonal);
        if report {
            println!("  v3 own {own:?}: rest {rest:?}");
        }
        GAINS.map(|g| (g, add(own, scaled(rest, g)))).to_vec()
    };
    let mut best = ([f64::MAX; 4], own, "own");
    let tried = [(0.0, CaModel::NONE, "none")]
        .into_iter()
        .chain(candidates(&analysis).map(|(g, m)| (g, m, "auto")))
        .chain(refined_own.into_iter().map(|(g, m)| (g, m, "own+")));
    for (g, candidate, kind) in tried {
        let c = parts(cost(candidate));
        if report {
            println!(
                "  v3 check {kind} {g} x {candidate:?}: {:.4}",
                c[0] / own_cost[0]
            );
        }
        if c[0] < best.0[0] {
            best = (c, candidate, kind);
        }
    }
    let keep = 1.0 - VERSUS_MARGIN;
    let ratios = [0, 1, 2, 3].map(|i| best.0[i] / own_cost[i]);
    let outer_share = own_cost[3] / own_cost[0];
    let clearer = ratios[..3].iter().all(|&r| r <= keep)
        && outer_share >= MIN_OUTER_SHARE
        && ratios[3] <= keep;
    // The patch shifts are measured only when the colour check is clear.
    let shifts = (clearer && own_patches.len() >= 12)
        .then(|| {
            let after = measure_patches(image, full, best.1, false);
            (after.len() >= 12).then(|| {
                (
                    shift_left(&own_patches, analysis.half_diagonal),
                    shift_left(&after, analysis.half_diagonal),
                )
            })
        })
        .flatten();
    let replaced = shifts.is_some_and(|(own_left, left)| {
        left <= MAX_SHIFT_SHARE * own_left && own_left - left >= MIN_SHIFT_GAIN
    });
    if report {
        println!(
            "  v3 choice: {} {:?} ({:.4}; halves {:.4} {:.4}; outer {:.4} of share {outer_share:.3}; shift left {shifts:?}; {} patches; {}; {:?})",
            best.2,
            best.1,
            ratios[0],
            ratios[1],
            ratios[2],
            ratios[3],
            full.len(),
            if replaced { "replaces" } else { "keeps own" },
            start.elapsed()
        );
    }
    replaced.then_some(best.1)
}

/// The shift in ±`MAX_SHIFT` with the lowest score: a coarse search in
/// quarter pixels, then a parabola through the best three.
pub(super) fn parabolic_minimum(score: &dyn Fn(f32) -> f32) -> Option<f32> {
    let mut best = (0.0f32, f32::MAX);
    let mut s = -MAX_SHIFT;
    while s <= MAX_SHIFT + 1e-6 {
        let v = score(s);
        if v < best.1 {
            best = (s, v);
        }
        s += 0.25;
    }
    if best.0.abs() >= MAX_SHIFT || best.1 == f32::MAX {
        return None;
    }
    let h = 0.25;
    let (l, c, r) = (score(best.0 - h), best.1, score(best.0 + h));
    let d = l - 2.0 * c + r;
    let offset = if d > 1e-9 { 0.5 * (l - r) / d * h } else { 0.0 };
    Some(best.0 + offset)
}
