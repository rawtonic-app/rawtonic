//! Small per-recipe data prepared once, rather than once per pixel and layer.
use crate::image_processing::{
    AllAdjustments, ColorGradeSettings, MAX_MASKS, MaskAdjustments, Point,
};

#[repr(C)]
#[derive(Clone, Copy, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct CurveSegment {
    endpoints: [f32; 4],
    coefficients: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct PreparedCurve {
    segments: [CurveSegment; 15],
    endpoints: [f32; 4],
    count: u32,
    bypass: u32,
    padding: [u32; 2],
    // Four packed segment indices per word. This is an exact search seed, not
    // sampled curve values: intervals containing several knots still scan on.
    segment_starts: [u32; 16],
}

#[repr(C)]
#[derive(Clone, Copy, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct PreparedLayer {
    curves: [PreparedCurve; 4],
    flags: [u32; 4],
}

impl PreparedLayer {
    pub(super) fn has_curves(&self) -> bool {
        self.flags[0] & 4 != 0
    }

    /// Exact curve data identifying this layer's display table.
    pub(super) fn curve_key(&self) -> &[u8] {
        bytemuck::bytes_of(&self.curves)
    }

    pub(super) fn set_curve_lut(&mut self, enabled: bool) {
        if enabled {
            self.flags[0] |= LAYER_CURVE_LUT;
        } else {
            self.flags[0] &= !LAYER_CURVE_LUT;
        }
    }
}

fn rgb_identity(points: &[Point; 16], count: usize) -> bool {
    count >= 2
        && points[..count].iter().all(|p| (p.x - p.y).abs() <= 0.5)
        && points[0].x.abs() < 0.1
        && points[0].y.abs() < 0.1
        && (points[count - 1].x - 255.0).abs() < 0.1
        && (points[count - 1].y - 255.0).abs() < 0.1
}

fn prepare_curve(points: &[Point; 16], count: u32, rgb: bool) -> PreparedCurve {
    let count = (count as usize).min(16);
    let mut prepared = PreparedCurve {
        count: count as u32,
        bypass: u32::from(count < 2 || (rgb && rgb_identity(points, count))),
        ..Default::default()
    };
    if count < 2 {
        return prepared;
    }
    let first = points[0];
    let last = points[count - 1];
    if !rgb && count == 2 && first.x == 0.0 && first.y == 0.0 && last.x == 255.0 && last.y == 255.0
    {
        // Luma identity still clamps HDR inputs; RGB bypass preserves them.
        prepared.bypass = 2;
    }
    prepared.endpoints = [first.x, first.y, last.x, last.y];
    if points[..count]
        .windows(2)
        .all(|pair| pair[0].x <= pair[1].x)
    {
        for bin in 0..64 {
            // Stay on the earlier segment at a rounded bin boundary. The
            // shader still tests the actual x against every following knot.
            // Bin zero also receives every negative input; moved endpoints
            // can leave valid negative segments that must not be skipped.
            let lower = if bin == 0 {
                f32::NEG_INFINITY
            } else {
                (bin as f32 * 255.0 / 64.0).next_down()
            };
            let start = (0..count - 1)
                .find(|i| points[i + 1].x >= lower)
                .unwrap_or(count - 2);
            prepared.segment_starts[bin / 4] |= (start as u32) << ((bin % 4) * 8);
        }
    }
    for i in 0..count - 1 {
        let p0 = points[i.saturating_sub(1)];
        let p1 = points[i];
        let p2 = points[i + 1];
        let p3 = points[(i + 2).min(count - 1)];
        let before = (p1.y - p0.y) / (p1.x - p0.x).max(0.001);
        let current = (p2.y - p1.y) / (p2.x - p1.x).max(0.001);
        let after = (p3.y - p2.y) / (p3.x - p2.x).max(0.001);
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
        let dx = p2.x - p1.x;
        let t1 = m1 * dx;
        let t2 = m2 * dx;
        prepared.segments[i] = CurveSegment {
            endpoints: [p1.x, p1.y, p2.x, p2.y],
            coefficients: [
                2.0 * p1.y + t1 - 2.0 * p2.y + t2,
                -3.0 * p1.y - 2.0 * t1 + 3.0 * p2.y - t2,
                t1,
                p1.y,
            ],
        };
    }
    prepared
}

fn has_scalars(m: &MaskAdjustments) -> bool {
    [
        m.exposure,
        m.brightness,
        m.contrast,
        m.highlights,
        m.shadows,
        m.whites,
        m.blacks,
        m.saturation,
        m.temperature,
        m.tint,
        m.vibrance,
        m.luma_noise_reduction,
        m.color_noise_reduction,
        m.clarity,
        m.dehaze,
        m.structure,
        m.glow_amount,
        m.halation_amount,
        m.flare_amount,
        m.hue,
        m.defringe,
        m.sharpness,
        m.moire[0],
    ]
    .iter()
    .any(|v| *v != 0.0)
        || m.hsl
            .iter()
            .any(|h| h.hue != 0.0 || h.saturation != 0.0 || h.luminance != 0.0)
}
/// Detail and effect sliders: summed per pixel only when a layer sets one.
fn has_detail(m: &MaskAdjustments) -> bool {
    [
        m.luma_noise_reduction,
        m.color_noise_reduction,
        m.clarity,
        m.dehaze,
        m.structure,
        m.glow_amount,
        m.halation_amount,
        m.flare_amount,
        m.defringe,
        m.sharpness,
        m.moire[0],
    ]
    .iter()
    .any(|v| *v != 0.0)
}
fn has_hsl(m: &MaskAdjustments) -> bool {
    m.hsl
        .iter()
        .any(|h| h.hue != 0.0 || h.saturation != 0.0 || h.luminance != 0.0)
}
fn has_grade(grades: [ColorGradeSettings; 4]) -> bool {
    grades
        .iter()
        .any(|g| g.saturation > 0.001 || g.luminance != 0.0)
}
fn curves(curves: [(&[Point; 16], u32); 4]) -> [PreparedCurve; 4] {
    std::array::from_fn(|i| prepare_curve(curves[i].0, curves[i].1, i != 0))
}

/// Samples per channel in a layer's display curve table (1,024 intervals).
pub(super) const CURVE_LUT_SAMPLES: usize = 1025;
// The photo's curve table (point_curve.rs) shares the layout.
const _: () = assert!(rawtonic_engine::point_curve::CURVE_TABLE_SAMPLES == CURVE_LUT_SAMPLES);
/// Layer flag: the shader may read this layer's curves from its table.
pub(super) const LAYER_CURVE_LUT: u32 = 8;
// A quarter of one 8-bit level. Combined with output rounding, a table read
// can move a display value by at most one level against the exact curves.
const CURVE_LUT_TOLERANCE: f32 = 0.25 / 255.0;

/// CPU mirror of the shader's `apply_curve_prepared`/`apply_luma_prepared`.
fn eval_curve(curve: &PreparedCurve, value: f32) -> f32 {
    match curve.bypass {
        1 => return value,
        2 => return value.clamp(0.0, 1.0),
        _ => {}
    }
    let [first_x, first_y, last_x, last_y] = curve.endpoints;
    let x = value * 255.0;
    if x <= first_x {
        return first_y / 255.0;
    }
    if x >= last_x {
        return last_y / 255.0;
    }
    for segment in &curve.segments[..(curve.count as usize).saturating_sub(1)] {
        let [x0, y0, x1, _] = segment.endpoints;
        if x <= x1 {
            if x1 <= x0 {
                return (y0 / 255.0).clamp(0.0, 1.0);
            }
            let [a, b, c, d] = segment.coefficients;
            let t = (x - x0) / (x1 - x0);
            return ((((a * t + b) * t + c) * t + d) / 255.0).clamp(0.0, 1.0);
        }
    }
    last_y / 255.0
}

/// Per-channel composite of a mask layer's curves, as `apply_curves_prepared`.
fn eval_layer_curves(layer: &PreparedLayer, value: f32) -> [f32; 3] {
    let luma = eval_curve(&layer.curves[0], value);
    std::array::from_fn(|c| eval_curve(&layer.curves[c + 1], luma).clamp(0.0, 1.0))
}

/// Same interpolation as the shader's `apply_curve_lut`.
fn read_curve_lut(table: &[[f32; 4]], value: f32, channel: usize) -> f32 {
    let position = value.clamp(0.0, 1.0) * (CURVE_LUT_SAMPLES - 1) as f32;
    let index = (position as usize).min(CURVE_LUT_SAMPLES - 2);
    let t = position - index as f32;
    table[index][channel] * (1.0 - t) + table[index + 1][channel] * t
}

/// Sampled display table for a mask layer's curves, or `None` when linear
/// interpolation cannot stay within tolerance (for example duplicate or
/// near-vertical knots). Mask-layer curve inputs are always within [0, 1].
pub(super) fn curve_lut(layer: &PreparedLayer) -> Option<Vec<[f32; 4]>> {
    let table: Vec<[f32; 4]> = (0..CURVE_LUT_SAMPLES)
        .map(|i| {
            let [r, g, b] = eval_layer_curves(layer, i as f32 / (CURVE_LUT_SAMPLES - 1) as f32);
            [r, g, b, 0.0]
        })
        .collect();
    let intervals = (CURVE_LUT_SAMPLES - 1) as f32;
    // Interior points of every interval, plus each knot where a clamp or a
    // degenerate segment can change slope abruptly.
    let interior = (0..CURVE_LUT_SAMPLES - 1)
        .flat_map(|i| [0.25, 0.5, 0.75].map(|f| (i as f32 + f) / intervals));
    let knots = layer.curves.iter().flat_map(|curve| {
        curve.segments[..(curve.count as usize).saturating_sub(1)]
            .iter()
            .flat_map(|s| [s.endpoints[0], s.endpoints[2]])
            .map(|x| x / 255.0)
            .filter(|x| (0.0..=1.0).contains(x))
            .collect::<Vec<_>>()
    });
    for value in interior.chain(knots) {
        let exact = eval_layer_curves(layer, value);
        for (channel, expected) in exact.iter().enumerate() {
            if (read_curve_lut(&table, value, channel) - expected).abs() > CURVE_LUT_TOLERANCE {
                return None;
            }
        }
    }
    Some(table)
}

pub(super) fn prepare_layers(a: &AllAdjustments) -> Vec<PreparedLayer> {
    let mut layers = vec![PreparedLayer::default(); MAX_MASKS + 1];
    let g = &a.global;
    layers[0].curves = curves([
        (&g.luma_curve, g.luma_curve_count),
        (&g.red_curve, g.red_curve_count),
        (&g.green_curve, g.green_curve_count),
        (&g.blue_curve, g.blue_curve_count),
    ]);
    for (layer, m) in layers[1..]
        .iter_mut()
        .zip(a.mask_adjustments.iter().take(a.mask_count as usize))
    {
        layer.curves = curves([
            (&m.luma_curve, m.luma_curve_count),
            (&m.red_curve, m.red_curve_count),
            (&m.green_curve, m.green_curve_count),
            (&m.blue_curve, m.blue_curve_count),
        ]);
        let luma_identity = m.luma_curve_count < 2
            || (m.luma_curve_count == 2
                && m.luma_curve[0].x == 0.0
                && m.luma_curve[0].y == 0.0
                && m.luma_curve[1].x == 255.0
                && m.luma_curve[1].y == 255.0);
        let has_curves = !luma_identity || layer.curves[1..].iter().any(|c| c.bypass == 0);
        layer.flags[0] = u32::from(has_scalars(m))
            | (u32::from(has_grade([
                m.color_grading_shadows,
                m.color_grading_midtones,
                m.color_grading_highlights,
                m.color_grading_global,
            ])) << 1)
            | (u32::from(has_curves) << 2)
            | (u32::from(m.grading_active != 0) << 4)
            | (u32::from(m.color_range_count != 0) << 5)
            | (u32::from(has_detail(m)) << 6)
            | (u32::from(has_hsl(m)) << 7);
    }
    // The global slot's second word: the steps any layer has.
    layers[0].flags[1] = layers[1..]
        .iter()
        .fold(0, |any, layer| any | layer.flags[0]);
    layers
}

/// Sliders the display shader sums per layer and pixel, in shader.wgsl's
/// `layer_sum` order: tone and colour, detail and effects, then HSL. Moiré is
/// left out: its step comes from the last layer that sets it, not a sum.
pub(super) const LAYER_SUM_PARAMS: usize = 46;
/// At most four vec4s of sums per pixel; more active sliders use the loop.
const LAYER_SUM_SLOTS: usize = 16;
/// vec4s before the pixel data (shader.wgsl `layer_sums`).
pub(super) const LAYER_SUMS_HEADER: usize = 6;

fn layer_sum_values(m: &MaskAdjustments) -> [f32; LAYER_SUM_PARAMS] {
    let mut values = [0.0; LAYER_SUM_PARAMS];
    let head = [
        m.exposure,
        m.brightness,
        m.contrast,
        m.highlights,
        m.shadows,
        m.whites,
        m.blacks,
        m.saturation,
        m.temperature,
        m.tint,
        m.vibrance,
        m.hue,
        m.luma_noise_reduction,
        m.color_noise_reduction,
        m.clarity,
        m.dehaze,
        m.structure,
        m.glow_amount,
        m.halation_amount,
        m.flare_amount,
        m.defringe,
        m.sharpness,
    ];
    values[..head.len()].copy_from_slice(&head);
    for (i, h) in m.hsl.iter().enumerate() {
        values[head.len() + i * 3..head.len() + i * 3 + 3].copy_from_slice(&[
            h.hue,
            h.saturation,
            h.luminance,
        ]);
    }
    values
}

/// Slider values of the layers the shader's first loop adds (flag 1), by
/// mask index; `None` when that loop must run: no such layer, or one with
/// moiré.
pub(super) type SummedLayers = Vec<(usize, [f32; LAYER_SUM_PARAMS])>;

pub(super) fn summed_layers(
    a: &AllAdjustments,
    prepared: &[PreparedLayer],
) -> Option<SummedLayers> {
    let summed: Vec<(usize, &MaskAdjustments)> = a
        .mask_adjustments
        .iter()
        .take(a.mask_count as usize)
        .enumerate()
        .filter(|(i, _)| prepared.get(i + 1).is_some_and(|l| l.flags[0] & 1 != 0))
        .collect();
    if summed.is_empty() || summed.iter().any(|(_, m)| m.moire[0] != 0.0) {
        return None;
    }
    Some(
        summed
            .into_iter()
            .map(|(index, m)| (index, layer_sum_values(m)))
            .collect(),
    )
}

/// What the layer sums pass computes (layer_sums.wgsl).
#[derive(Clone, PartialEq)]
pub(super) struct LayerSumsPlan {
    /// The sliders some summed layer sets, one bit each; a slider's slot is
    /// the number of set bits before its own.
    pub(super) bits: [u32; 2],
    /// Per summed layer: its mask index (bits), then `vec4_count` vec4s.
    pub(super) layers: Vec<[f32; 4]>,
    pub(super) layer_count: u32,
    pub(super) vec4_count: u32,
    /// The layer left out of the sums (a slider moving on it) and its values
    /// in the slots: the shader adds it per pixel.
    pub(super) exclude: Option<(usize, [[f32; 4]; 4])>,
}

/// The sums of `summed` without layer `exclude`, or `None` when more
/// sliders are active than `LAYER_SUM_SLOTS`. The slots cover every summed
/// layer's sliders, the left-out one's too.
pub(super) fn plan_layer_sums(
    summed: &SummedLayers,
    exclude: Option<usize>,
) -> Option<LayerSumsPlan> {
    let active: Vec<usize> = (0..LAYER_SUM_PARAMS)
        .filter(|param| summed.iter().any(|(_, v)| v[*param] != 0.0))
        .collect();
    if active.is_empty() || active.len() > LAYER_SUM_SLOTS {
        return None;
    }
    let vec4_count = active.len().div_ceil(4);
    let mut bits = [0u32; 2];
    for param in &active {
        bits[param / 32] |= 1 << (param % 32);
    }
    let pack = |v: &[f32; LAYER_SUM_PARAMS]| {
        let mut packed = [[0.0; 4]; 4];
        for (slot, param) in active.iter().enumerate() {
            packed[slot / 4][slot % 4] = v[*param];
        }
        packed
    };
    let mut layers = Vec::with_capacity(summed.len() * (vec4_count + 1));
    let mut layer_count = 0;
    let mut left_out = None;
    for (index, v) in summed {
        if Some(*index) == exclude {
            left_out = Some((*index, pack(v)));
            continue;
        }
        layers.push([f32::from_bits(*index as u32), 0.0, 0.0, 0.0]);
        layers.extend_from_slice(&pack(v)[..vec4_count]);
        layer_count += 1;
    }
    Some(LayerSumsPlan {
        bits,
        layers,
        layer_count,
        vec4_count: vec4_count as u32,
        exclude: left_out,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(x: f32, y: f32) -> Point {
        let mut point = Point::default();
        point.x = x;
        point.y = y;
        point
    }

    fn selected_segment(curve: &PreparedCurve, value: f32, indexed: bool) -> usize {
        let x = value * 255.0;
        if x <= curve.endpoints[0] {
            return usize::MAX;
        }
        if x >= curve.endpoints[2] {
            return usize::MAX - 1;
        }
        let start = if indexed {
            let bin = (value * 64.0).clamp(0.0, 63.0) as usize;
            ((curve.segment_starts[bin / 4] >> ((bin % 4) * 8)) & 255) as usize
        } else {
            0
        };
        (start..curve.count as usize - 1)
            .find(|index| x <= curve.segments[*index].endpoints[2])
            .unwrap_or(usize::MAX - 1)
    }

    fn assert_same_segment(curve: &PreparedCurve, values: impl IntoIterator<Item = f32>) {
        for value in values {
            assert_eq!(
                selected_segment(curve, value, true),
                selected_segment(curve, value, false),
                "indexed search must preserve first matching segment for {value:?}"
            );
        }
    }

    #[test]
    fn prepared_curve_seeds_preserve_bin_boundaries_dense_and_duplicate_knots() {
        for bin in 1..64 {
            let boundary = bin as f32 / 64.0;
            let lower = bin as f32 * 255.0 / 64.0;
            for duplicates in [false, true] {
                let points: [Point; 16] = std::array::from_fn(|index| {
                    let x = match index {
                        0 => 0.0,
                        15 => 255.0,
                        _ => {
                            let offset = if duplicates { index / 2 } else { index };
                            f32::from_bits(lower.to_bits() - 7 + offset as u32)
                        }
                    };
                    point(x, if index % 2 == 0 { 31.0 } else { 224.0 })
                });
                let curve = prepare_curve(&points, 16, false);
                let mut values = vec![-10.0, 0.0, 1.0, 10.0];
                for center in
                    std::iter::once(boundary).chain(points.iter().map(|point| point.x / 255.0))
                {
                    values.extend([
                        center.next_down().next_down(),
                        center.next_down(),
                        center,
                        center.next_up(),
                        center.next_up().next_up(),
                    ]);
                }
                assert_same_segment(&curve, values);
                // Also cover bins on either side of the tightly packed cluster.
                assert_same_segment(&curve, (0..=1024).map(|step| step as f32 / 1024.0));
            }
        }
    }

    #[test]
    fn prepared_curve_seeds_keep_unsorted_and_moved_endpoint_search_semantics() {
        for xs in [
            [-32.0, 40.0, 12.0, 128.0, 90.0, 300.0],
            [-255.0, -255.0, -100.0, 256.0, 256.0, 510.0],
            [200.0, 160.0, 128.0, 80.0, 30.0, 10.0],
        ] {
            let mut points = [Point::default(); 16];
            for (index, x) in xs.into_iter().enumerate() {
                points[index] = point(x, index as f32 * 37.0);
            }
            let curve = prepare_curve(&points, xs.len() as u32, false);
            if xs.windows(2).any(|pair| pair[0] > pair[1]) {
                assert_eq!(
                    curve.segment_starts, [0; 16],
                    "unsorted curves must scan from the start"
                );
            }
            assert_same_segment(&curve, (-2048..=3072).map(|step| step as f32 / 1024.0));
            for x in xs {
                let value = x / 255.0;
                assert_same_segment(&curve, [value.next_down(), value, value.next_up()]);
            }
        }
    }

    fn layer(curves: [&[(f32, f32)]; 4]) -> PreparedLayer {
        let mut layer = PreparedLayer::default();
        for (channel, knots) in curves.into_iter().enumerate() {
            let mut points = [Point::default(); 16];
            for (index, (x, y)) in knots.iter().enumerate() {
                points[index] = point(*x, *y);
            }
            layer.curves[channel] = prepare_curve(&points, knots.len() as u32, channel != 0);
        }
        layer
    }

    #[test]
    fn curve_tables_match_exact_curves_between_every_check_point() {
        let luma = [(0.0, 8.0), (55.0, 43.0), (165.0, 191.0), (255.0, 248.0)];
        let identity = [(0.0, 0.0), (255.0, 255.0)];
        for curves in [
            [
                &luma[..],
                &[(0.0, 0.0), (128.0, 138.0), (255.0, 255.0)][..],
                &[(0.0, 0.0), (128.0, 118.0), (255.0, 255.0)],
                &[(0.0, 4.0), (128.0, 128.0), (255.0, 251.0)],
            ],
            // Luma identity clamps; untouched RGB channels bypass.
            [
                &identity[..],
                &[(0.0, 20.0), (90.0, 60.0), (200.0, 230.0), (255.0, 240.0)],
                &identity,
                &identity,
            ],
            // Moved endpoints and a strong but smooth S curve.
            [
                &[(20.0, 0.0), (128.0, 200.0), (235.0, 255.0)][..],
                &identity,
                &[(0.0, 255.0), (255.0, 0.0)],
                &identity,
            ],
        ] {
            let layer = layer(curves);
            let table = curve_lut(&layer).expect("smooth curves fit the table");
            // Dense points that are not the table's own check points.
            for step in 0..=200_003 {
                let value = step as f32 / 200_003.0;
                let exact = eval_layer_curves(&layer, value);
                for (channel, expected) in exact.into_iter().enumerate() {
                    let delta = (read_curve_lut(&table, value, channel) - expected).abs();
                    assert!(
                        delta <= CURVE_LUT_TOLERANCE,
                        "{delta} at {value}, channel {channel}"
                    );
                }
            }
        }
    }

    #[test]
    fn curve_tables_refuse_steps_and_near_vertical_segments() {
        let identity = [(0.0, 0.0), (255.0, 255.0)];
        for luma in [
            // Duplicate knot: a discontinuity cannot be interpolated.
            &[(0.0, 0.0), (100.0, 0.0), (100.0, 255.0), (255.0, 255.0)][..],
            // A rise over a fraction of one table interval.
            &[(0.0, 0.0), (100.0, 0.0), (100.05, 255.0), (255.0, 255.0)],
        ] {
            assert!(curve_lut(&layer([luma, &identity, &identity, &identity])).is_none());
        }
    }
}
