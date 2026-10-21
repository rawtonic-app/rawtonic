//! Point curves with more control points than the shader's 16 (Lightroom
//! curves imported with many points). The renderer then reads the global
//! curves from one sampled table (`curve_table`) instead of the uniform's
//! first 16 points; recipes whose curves fit stay on the exact evaluator.
use serde_json::Value;
use std::sync::Arc;

/// Recipe `curveVersion` from which curves with more than 16 points render
/// whole; before, the renderer used their first 16 points.
pub const CURVE_VERSION: u64 = 2;
/// Control points one curve channel holds in the uniforms (shader `Point` arrays).
pub const SHADER_CURVE_POINTS: usize = 16;
/// Samples per channel in a curve table (1,024 intervals, shader `apply_curve_lut`).
pub const CURVE_TABLE_SAMPLES: usize = 1025;

/// A curve channel's points as the uniforms read them: entries without a
/// numeric x or y count as (0, 0) (`convert_points_to_aligned`).
pub fn curve_points(points: &[Value]) -> Vec<(f32, f32)> {
    points
        .iter()
        .map(|p| match (p["x"].as_f64(), p["y"].as_f64()) {
            (Some(x), Some(y)) => (x as f32, y as f32),
            _ => (0.0, 0.0),
        })
        .collect()
}

fn hermite(x: f32, p1: (f32, f32), p2: (f32, f32), m1: f32, m2: f32) -> f32 {
    let dx = p2.0 - p1.0;
    if dx <= 0.0 {
        return p1.1;
    }
    let t = (x - p1.0) / dx;
    let t2 = t * t;
    let t3 = t2 * t;
    let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
    let h10 = t3 - 2.0 * t2 + t;
    let h01 = -2.0 * t3 + 3.0 * t2;
    let h11 = t3 - t2;
    h00 * p1.1 + h10 * m1 * dx + h01 * p2.1 + h11 * m2 * dx
}

/// The shader's `apply_curve` (monotone cubic Hermite through the points,
/// flat beyond the ends) for any number of points; `value` in 0–1.
pub fn curve_value(points: &[(f32, f32)], value: f32) -> f32 {
    let count = points.len();
    if count < 2 {
        return value;
    }
    let x = value * 255.0;
    if x <= points[0].0 {
        return points[0].1 / 255.0;
    }
    if x >= points[count - 1].0 {
        return points[count - 1].1 / 255.0;
    }
    for i in 0..count - 1 {
        let p1 = points[i];
        let p2 = points[i + 1];
        if x <= p2.0 {
            let p0 = points[i.saturating_sub(1)];
            let p3 = points[(i + 2).min(count - 1)];
            let before = (p1.1 - p0.1) / (p1.0 - p0.0).max(0.001);
            let current = (p2.1 - p1.1) / (p2.0 - p1.0).max(0.001);
            let after = (p3.1 - p2.1) / (p3.0 - p2.0).max(0.001);
            let mut m1 = if i == 0 {
                current
            } else if before * current <= 0.0 {
                0.0
            } else {
                (before + current) / 2.0
            };
            let mut m2 = if i + 1 == count - 1 {
                current
            } else if current * after <= 0.0 {
                0.0
            } else {
                (current + after) / 2.0
            };
            if current != 0.0 {
                let alpha = m1 / current;
                let beta = m2 / current;
                if alpha * alpha + beta * beta > 9.0 {
                    let tau = 3.0 / (alpha * alpha + beta * beta).sqrt();
                    m1 *= tau;
                    m2 *= tau;
                }
            }
            return (hermite(x, p1, p2, m1, m2) / 255.0).clamp(0.0, 1.0);
        }
    }
    points[count - 1].1 / 255.0
}

/// The shader's `is_default_curve`: a channel curve that leaves values alone.
fn is_identity(points: &[(f32, f32)]) -> bool {
    let (Some(first), Some(last)) = (points.first(), points.last()) else {
        return false;
    };
    points.len() >= 2
        && points.iter().all(|p| (p.0 - p.1).abs() <= 0.5)
        && first.0.abs() < 0.1
        && first.1.abs() < 0.1
        && (last.0 - 255.0).abs() < 0.1
        && (last.1 - 255.0).abs() < 0.1
}

/// The shader's `apply_all_curves` for one value in 0–1: the luma (RGB)
/// curve on every channel, then each channel's own curve.
pub fn curves_value(channels: &[Vec<(f32, f32)>; 4], value: f32) -> [f32; 3] {
    let luma = curve_value(&channels[0], value);
    std::array::from_fn(|c| {
        let own = &channels[c + 1];
        let v = if is_identity(own) {
            luma
        } else {
            curve_value(own, luma)
        };
        v.clamp(0.0, 1.0)
    })
}

/// The global curves sampled for the renderer when a channel has more points
/// than the uniforms hold (`None` otherwise): `channels` are the luma, red,
/// green and blue points as the recipe stores them. The table holds the exact
/// curves every 1/1024 of the input and is read linearly between, so a read
/// lies between the exact curves' values within a quarter input level: on
/// smooth curves within 0.01 of an output level, while a near-vertical
/// segment (a jump within one input level) is spread over that quarter level.
pub fn curve_table(channels: &[Vec<(f32, f32)>; 4]) -> Option<Arc<Vec<[f32; 4]>>> {
    if channels.iter().all(|c| c.len() <= SHADER_CURVE_POINTS) {
        return None;
    }
    let table = (0..CURVE_TABLE_SAMPLES)
        .map(|i| {
            let [r, g, b] = curves_value(channels, i as f32 / (CURVE_TABLE_SAMPLES - 1) as f32);
            [r, g, b, 0.0]
        })
        .collect();
    Some(Arc::new(table))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> Vec<(f32, f32)> {
        vec![(0.0, 0.0), (255.0, 255.0)]
    }

    /// The shader's linear table read (`apply_curve_lut`).
    fn read(table: &[[f32; 4]], value: f32, channel: usize) -> f32 {
        let position = value.clamp(0.0, 1.0) * (CURVE_TABLE_SAMPLES - 1) as f32;
        let index = (position as usize).min(CURVE_TABLE_SAMPLES - 2);
        let t = position - index as f32;
        table[index][channel] * (1.0 - t) + table[index + 1][channel] * t
    }

    // A Lightroom-style curve with 20 points: zig-zag of ±10 levels.
    fn long_curve() -> Vec<(f32, f32)> {
        let xs = [
            0, 13, 27, 40, 54, 67, 81, 94, 107, 121, 134, 148, 161, 174, 188, 201, 215, 228, 242,
            255,
        ];
        xs.iter()
            .enumerate()
            .map(|(i, &x)| {
                let y = if i == 0 || i == xs.len() - 1 {
                    x
                } else if i % 2 == 1 {
                    x + 10
                } else {
                    x - 10
                };
                (x as f32, y as f32)
            })
            .collect()
    }

    #[test]
    fn short_curves_keep_the_exact_evaluator() {
        let sixteen: Vec<_> = (0..16)
            .map(|i| (i as f32 * 17.0, i as f32 * 17.0))
            .collect();
        assert!(curve_table(&[sixteen, identity(), identity(), identity()]).is_none());
    }

    #[test]
    fn long_curves_pass_through_every_point() {
        let curve = long_curve();
        let table = curve_table(&[curve.clone(), identity(), identity(), identity()]).unwrap();
        for &(x, y) in &curve {
            for channel in 0..3 {
                let got = read(&table, x / 255.0, channel) * 255.0;
                assert!((got - y).abs() < 0.05, "x {x}: {got} vs {y}");
            }
        }
        // Past point 16 (x 201) the curve still follows its points; the
        // first 16 alone would hold 211 from there on.
        assert!((read(&table, 242.0 / 255.0, 0) * 255.0 - 232.0).abs() < 0.05);
    }

    #[test]
    fn table_reads_stay_within_a_quarter_level_of_the_curve() {
        let mut red = long_curve();
        red.reverse();
        let red: Vec<_> = red.iter().map(|&(x, y)| (255.0 - x, 255.0 - y)).collect();
        let channels = [long_curve(), red, identity(), long_curve()];
        let table = curve_table(&channels).unwrap();
        let mut worst = 0.0f32;
        for i in 0..=8160 {
            let v = i as f32 / 8160.0;
            let exact = curves_value(&channels, v);
            for (c, expected) in exact.iter().enumerate() {
                worst = worst.max((read(&table, v, c) - expected).abs() * 255.0);
            }
        }
        assert!(worst < 0.25, "worst {worst} levels");
    }

    #[test]
    fn steep_curves_stay_within_a_quarter_input_level() {
        // A jump from 0 to 255 between inputs 128 and 129, 20 points.
        let xs = [
            0, 10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120, 128, 129, 150, 180, 210, 240, 255,
        ];
        let steep: Vec<_> = xs
            .iter()
            .map(|&x| (x as f32, if x <= 128 { 0.0 } else { 255.0 }))
            .collect();
        let channels = [steep, identity(), identity(), identity()];
        let table = curve_table(&channels).unwrap();
        let reach = 1.0 / (CURVE_TABLE_SAMPLES - 1) as f32;
        for i in 0..=65_280 {
            let v = i as f32 / 65_280.0;
            let got = read(&table, v, 0);
            let (lo, hi) = (
                curves_value(&channels, (v - reach).max(0.0))[0],
                curves_value(&channels, (v + reach).min(1.0))[0],
            );
            assert!(
                got >= lo.min(hi) - 1e-5 && got <= lo.max(hi) + 1e-5,
                "{v}: {got} not in {lo}..{hi}"
            );
        }
    }

    #[test]
    fn matches_the_sixteen_point_evaluation_on_short_curves() {
        // The same arithmetic as the shader: a 3-point curve's midpoint and
        // the flat extension beyond moved endpoints.
        let curve = vec![(32.0, 0.0), (128.0, 160.0), (224.0, 255.0)];
        assert_eq!(curve_value(&curve, 128.0 / 255.0), 160.0 / 255.0);
        assert_eq!(curve_value(&curve, 0.05), 0.0);
        assert_eq!(curve_value(&curve, 0.95), 1.0);
    }
}
