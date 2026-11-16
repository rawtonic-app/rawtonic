//! The value of rank `index` among `values` in `f32::total_cmp` order, found
//! in parallel. Values equal under `total_cmp` have the same bits, so the
//! value at a rank does not depend on the order of `values` or on how the
//! work is split: this returns bit for bit what
//! `values.select_nth_unstable_by(index, f32::total_cmp)` would, without its
//! serial pass over millions of samples (the decode measures, notes:
//! benchmark-results/sonyr-import-region-speed-2026-11-16).

use rayon::prelude::*;

/// Below this many values one serial selection is cheaper.
const PARALLEL_FROM: usize = 1 << 16;
/// Histogram bins: the key's high 16 bits.
const BINS: usize = 1 << 16;

/// `f32::total_cmp`'s key as an unsigned integer in the same order.
fn key(value: f32) -> u32 {
    let bits = value.to_bits() as i32;
    let ordered = bits ^ ((((bits >> 31) as u32) >> 1) as i32);
    (ordered as u32) ^ 0x8000_0000
}

/// The value of rank `index` in `f32::total_cmp` order among all values of
/// `parts` taken together (their order and split do not matter). Panics when
/// `index` is out of range, as `select_nth_unstable_by`.
pub fn nth<P: AsRef<[f32]> + Sync>(parts: &[P], index: usize) -> f32 {
    let len: usize = parts.iter().map(|part| part.as_ref().len()).sum();
    assert!(index < len, "rank {index} of {len}");
    if len < PARALLEL_FROM {
        let mut values: Vec<f32> = parts.iter().flat_map(|p| p.as_ref()).copied().collect();
        return *values.select_nth_unstable_by(index, f32::total_cmp).1;
    }
    // One histogram per thread over a slice of about the same size each.
    let slices: Vec<&[f32]> = {
        let size = len.div_ceil(rayon::current_num_threads().max(1));
        parts
            .iter()
            .flat_map(|part| part.as_ref().chunks(size.max(1)))
            .collect()
    };
    let groups = slices.chunks(slices.len().div_ceil(rayon::current_num_threads().max(1)));
    let histogram = groups
        .collect::<Vec<_>>()
        .into_par_iter()
        .map(|group| {
            let mut counts = vec![0u32; BINS];
            for &value in group.iter().flat_map(|slice| slice.iter()) {
                counts[(key(value) >> 16) as usize] += 1;
            }
            counts
        })
        .reduce(
            || vec![0u32; BINS],
            |mut a, b| {
                a.iter_mut().zip(b).for_each(|(a, b)| *a += b);
                a
            },
        );
    let mut below = 0;
    let mut bin = 0;
    for (i, &count) in histogram.iter().enumerate() {
        if below + count as usize > index {
            bin = i as u32;
            break;
        }
        below += count as usize;
    }
    let mut candidates: Vec<f32> = slices
        .par_iter()
        .flat_map_iter(|slice| slice.iter().copied().filter(|&v| key(v) >> 16 == bin))
        .collect();
    *candidates
        .select_nth_unstable_by(index - below, f32::total_cmp)
        .1
}

#[cfg(test)]
mod tests {
    use super::{key, nth};

    #[test]
    fn the_key_orders_as_total_cmp() {
        let values = [
            f32::NEG_INFINITY,
            -f32::NAN,
            -1.0,
            -0.0,
            0.0,
            f32::MIN_POSITIVE,
            1e-40,
            0.5,
            f32::INFINITY,
            f32::NAN,
        ];
        for a in values {
            for b in values {
                assert_eq!(key(a).cmp(&key(b)), a.total_cmp(&b), "{a} {b}");
            }
        }
    }

    #[test]
    fn the_same_value_as_a_serial_selection() {
        let mut state = 11u32;
        let mut next = || {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            state
        };
        for len in [1usize, 100, 70_000, 300_001] {
            let values: Vec<f32> = (0..len)
                .map(|i| match next() % 50 {
                    0 => f32::NAN,
                    1 => -0.0,
                    2 => 0.0,
                    3 => f32::INFINITY,
                    // Many ties in one histogram bin.
                    4..=20 => 0.25,
                    _ => f32::from_bits(next()) * if i % 2 == 0 { 1.0 } else { -1.0 },
                })
                .collect();
            for index in [0, len / 4, len / 2, len * 999 / 1000, len - 1] {
                let mut serial = values.clone();
                let expected = *serial.select_nth_unstable_by(index, f32::total_cmp).1;
                // Whole, and split unevenly into parts.
                let (a, b) = values.split_at(len / 3);
                let (b, c) = b.split_at(b.len() / 5);
                assert_eq!(
                    nth(&[a, b, c], index).to_bits(),
                    expected.to_bits(),
                    "len {len} rank {index} in parts"
                );
                assert_eq!(
                    nth(&[&values], index).to_bits(),
                    expected.to_bits(),
                    "len {len} rank {index}"
                );
            }
        }
    }
}
