//! Output sharpening as it was before its speed-up (2026-11-01), kept
//! verbatim as the reference the faster `sharpen` must match bit for bit
//! (tests in `output_sharpening`). Only the `super::` import is new.

use super::*;

pub(super) fn sharpen<T: Sample>(
    data: &mut [T],
    channels: usize,
    width: usize,
    height: usize,
    max: f32,
    settings: &OutputSharpening,
) {
    if width == 0 || height == 0 {
        return;
    }
    let weights = kernel(settings.radius);
    let reach = weights.len() / 2;
    let scale = 1.0 / max;
    let stride = width * channels;
    // Horizontal pass of the lightness; the vertical pass runs per output
    // row, so one plane is all the extra memory.
    let mut horizontal = vec![0.0f32; width * height];
    horizontal
        .par_chunks_mut(width)
        .zip(data.par_chunks(stride))
        .for_each(|(out, row)| {
            let light: Vec<f32> = row
                .chunks_exact(channels)
                .map(|px| lightness(px, scale))
                .collect();
            for (x, value) in out.iter_mut().enumerate() {
                *value = weights
                    .iter()
                    .enumerate()
                    .map(|(k, w)| {
                        let xi = (x + k).saturating_sub(reach).min(width - 1);
                        w * light[xi]
                    })
                    .sum();
            }
        });
    let amount = settings.amount / 100.0;
    let threshold = settings.threshold / 255.0;
    data.par_chunks_mut(stride)
        .enumerate()
        .for_each(|(y, row)| {
            for x in 0..width {
                let blurred: f32 = weights
                    .iter()
                    .enumerate()
                    .map(|(k, w)| {
                        let yi = (y + k).saturating_sub(reach).min(height - 1);
                        w * horizontal[yi * width + x]
                    })
                    .sum();
                let px = &mut row[x * channels..x * channels + channels];
                let detail = lightness(px, scale) - blurred;
                // Below the threshold nothing changes; above it the full
                // detail returns over a short ramp (no hard step).
                let gate = if threshold > 0.0 {
                    ((detail.abs() - threshold) / threshold).clamp(0.0, 1.0)
                } else {
                    1.0
                };
                let delta = amount * detail * gate * max;
                if delta != 0.0 {
                    for value in px.iter_mut().take(3) {
                        *value = T::from_f32(value.to_f32() + delta, max);
                    }
                }
            }
        });
}
