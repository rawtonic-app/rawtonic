//! Hot and dead pixel suppression as it was before its row prefilter
//! (2026-11-15), kept verbatim as the reference the faster `suppress` must
//! match exactly (tests in `hot_pixels`). Only the `super::` imports differ.

use super::*;

#[allow(clippy::too_many_arguments)]
pub fn suppress<T: Copy + Send + Sync>(
    data: &mut [T],
    width: usize,
    active: (usize, usize, usize, usize),
    cfa: &CFA,
    wb: [f32; 4],
    dead_test: DeadTest,
    level: impl Fn(T) -> f32 + Sync,
    store: impl Fn(f32) -> T + Sync,
) -> usize {
    let Some(neighbours) = Neighbours::new(cfa) else {
        return 0;
    };
    let (left, top, aw, ah) = active;
    if aw < 8 || ah < 8 || data.len() < (top + ah) * width {
        return 0;
    }
    let wb: [f32; 3] = std::array::from_fn(|i| {
        if wb[i] > 0.0 && wb[i].is_finite() {
            wb[i]
        } else {
            1.0
        }
    });
    let source: &[T] = data;
    let fixes: Vec<(usize, T)> = (top + 2..top + ah - 2)
        .into_par_iter()
        .flat_map_iter(|row| {
            let mut out = Vec::new();
            let mut values = [0f32; 24];
            for col in left + 2..left + aw - 2 {
                let phase = (row % neighbours.period) * neighbours.period + col % neighbours.period;
                let colour = cfa.color_at(row, col);
                if colour >= 3 {
                    continue;
                }
                let v = level(source[row * width + col]);
                let same = &neighbours.same[phase];
                let mut lo = f32::INFINITY;
                let mut hi = f32::NEG_INFINITY;
                for (i, (dy, dx)) in same.iter().enumerate() {
                    let n = level(
                        source[(row as isize + dy) as usize * width + (col as isize + dx) as usize],
                    );
                    values[i] = n;
                    lo = lo.min(n);
                    hi = hi.max(n);
                }
                // Adjacent samples of the other colours, white-balanced.
                let adjacent = || {
                    neighbours.other[phase].iter().map(|(dy, dx, c)| {
                        level(
                            source[(row as isize + dy) as usize * width
                                + (col as isize + dx) as usize],
                        ) * wb[*c]
                            / wb[colour]
                    })
                };
                let hot = v > HOT_RATIO * hi.max(0.0) + HOT_MARGIN
                    && v > HOT_NEIGHBOUR_RATIO * adjacent().fold(0.0f32, f32::max) + HOT_MARGIN;
                let dead = !hot
                    && lo > DEAD_FLOOR
                    && v < DEAD_RATIO * lo - DEAD_MARGIN
                    && (dead_test == DeadTest::SameColour
                        || v < DEAD_NEIGHBOUR_RATIO * adjacent().fold(f32::INFINITY, f32::min)
                            - DEAD_MARGIN);
                if hot || dead {
                    let n = same.len();
                    let slice = &mut values[..n];
                    slice.sort_by(f32::total_cmp);
                    let median = 0.5 * (slice[(n - 1) / 2] + slice[n / 2]);
                    out.push((row * width + col, store(median)));
                }
            }
            out
        })
        .collect();
    for (index, value) in &fixes {
        data[*index] = *value;
    }
    fixes.len()
}
