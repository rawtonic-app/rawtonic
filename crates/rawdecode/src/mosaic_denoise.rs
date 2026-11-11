// SPDX-License-Identifier: AGPL-3.0-or-later
//! A hook for denoisers that work on the Bayer mosaic, before demosaicing.
//!
//! `develop_raw_image` hands the active area to the denoiser as
//! black-subtracted, white-normalised samples (1.0 = the sensor's white;
//! clipped samples can exceed it) and blends the result back by `amount`.
//! Samples at or above white are kept as recorded. Only 2 × 2 RGB Bayer
//! mosaics are passed on; other sensors (X-Trans, linear DNGs, Foveon) and
//! fast developments are left alone.
//!
//! A network can shift the level and colour of what it denoises: Nonlocal
//! MatchFilter's result is darker than the sensor's samples in deep
//! shadows at high ISO, by a different amount per colour, and tints the
//! shadows of cameras it was not trained on (notes:
//! benchmark-results/aid-ai-denoise-shadows-2026-11-11/). With
//! `sensor_means` the result keeps the sensor's local means (`keep_means`):
//! per 2 × 2 position the local mean of sample − result is added back, so
//! the network removes noise and does not move an area's level or colour.

use std::sync::Arc;

use anyhow::Result;
use rayon::prelude::*;

/// A denoiser for a 2 × 2 Bayer mosaic.
pub trait MosaicDenoiser: Send + Sync {
    /// Denoised samples in the layout of `mosaic` (row-major, `width` per
    /// row). `pattern` holds the colours of the 2 × 2 tile at the origin in
    /// row order (0 red, 1 green, 2 blue).
    fn denoise(
        &self,
        mosaic: &[f32],
        width: usize,
        height: usize,
        pattern: [u8; 4],
    ) -> Result<Vec<f32>>;

    /// `denoise` for noise `noise_scale` times the photo's: below 1 the
    /// network keeps more, above 1 it smooths more. Denoisers without such
    /// a control ignore it.
    fn denoise_scaled(
        &self,
        mosaic: &[f32],
        width: usize,
        height: usize,
        pattern: [u8; 4],
        noise_scale: f32,
    ) -> Result<Vec<f32>> {
        let _ = noise_scale;
        self.denoise(mosaic, width, height, pattern)
    }

    /// `denoise_scaled` that reports `progress(done, total)` as it goes (AI
    /// Denoise: once per tile). Denoisers without steps report nothing.
    fn denoise_reporting(
        &self,
        mosaic: &[f32],
        width: usize,
        height: usize,
        pattern: [u8; 4],
        noise_scale: f32,
        progress: &(dyn Fn(usize, usize) + Sync),
    ) -> Result<Vec<f32>> {
        let _ = progress;
        self.denoise_scaled(mosaic, width, height, pattern, noise_scale)
    }
}

/// Told `(done, total)` while a denoiser works, e.g. for an export's progress.
pub type DenoiseProgress = Arc<dyn Fn(usize, usize) + Send + Sync>;

/// A denoiser and how much of its result to keep (0 … 1).
#[derive(Clone)]
pub struct MosaicDenoise {
    pub denoiser: Arc<dyn MosaicDenoiser>,
    pub amount: f32,
    /// The noise the denoiser is told about, relative to the photo's
    /// (`MosaicDenoiser::denoise_scaled`); 1 for AI Denoise version 1.
    pub noise_scale: f32,
    /// Whether the result keeps the sensor's local means (`keep_means`;
    /// AI Denoise from version 4).
    pub sensor_means: bool,
    /// Set once a development ran the denoiser (it skips non-Bayer sensors).
    pub applied: Arc<std::sync::atomic::AtomicBool>,
    /// Where the denoiser reports how far it is, if anywhere.
    pub progress: Option<DenoiseProgress>,
}

impl MosaicDenoise {
    pub fn new(denoiser: Arc<dyn MosaicDenoiser>, amount: f32) -> Self {
        Self {
            denoiser,
            amount,
            noise_scale: 1.0,
            sensor_means: false,
            applied: Arc::default(),
            progress: None,
        }
    }

    pub fn with_progress(mut self, progress: DenoiseProgress) -> Self {
        self.progress = Some(progress);
        self
    }

    pub fn with_noise_scale(mut self, noise_scale: f32) -> Self {
        self.noise_scale = noise_scale;
        self
    }

    pub fn with_sensor_means(mut self, sensor_means: bool) -> Self {
        self.sensor_means = sensor_means;
        self
    }

    /// Whether a development ran the denoiser.
    pub fn was_applied(&self) -> bool {
        self.applied.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl std::fmt::Debug for MosaicDenoise {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MosaicDenoise")
            .field("amount", &self.amount)
            .field("noise_scale", &self.noise_scale)
            .field("sensor_means", &self.sensor_means)
            .finish_non_exhaustive()
    }
}

/// Runs `denoise` on the active area of `data` (samples in the decoder's
/// units, mapped to normalised levels and back by `level` and `store`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply<T: Copy>(
    denoise: &MosaicDenoise,
    data: &mut [T],
    width: usize,
    active: (usize, usize, usize, usize),
    pattern: [u8; 4],
    level: impl Fn(T) -> f32,
    store: impl Fn(f32) -> T,
) -> Result<()> {
    let amount = denoise.amount.clamp(0.0, 1.0);
    if amount <= 0.0 {
        return Ok(());
    }
    let (left, top, aw, ah) = active;
    // Whole 2 × 2 tiles only, so the pattern stays aligned.
    let (aw, ah) = (aw & !1, ah & !1);
    if aw < 2 || ah < 2 {
        return Ok(());
    }
    let mut mosaic = Vec::with_capacity(aw * ah);
    for y in top..top + ah {
        let row = &data[y * width + left..y * width + left + aw];
        mosaic.extend(row.iter().map(|&v| level(v)));
    }
    let denoised = if let Some(progress) = &denoise.progress {
        denoise.denoiser.denoise_reporting(
            &mosaic,
            aw,
            ah,
            pattern,
            denoise.noise_scale,
            progress.as_ref(),
        )?
    } else if denoise.noise_scale == 1.0 {
        denoise.denoiser.denoise(&mosaic, aw, ah, pattern)?
    } else {
        denoise
            .denoiser
            .denoise_scaled(&mosaic, aw, ah, pattern, denoise.noise_scale)?
    };
    anyhow::ensure!(
        denoised.len() == mosaic.len(),
        "denoiser returned {} samples for {}",
        denoised.len(),
        mosaic.len()
    );
    denoise
        .applied
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let mut denoised = denoised;
    if denoise.sensor_means {
        keep_means(&mosaic, &mut denoised, aw, ah);
    }
    for y in 0..ah {
        let row = &mut data[(top + y) * width + left..(top + y) * width + left + aw];
        for (x, sample) in row.iter_mut().enumerate() {
            let original = mosaic[y * aw + x];
            if original >= 1.0 {
                continue;
            }
            let value = original + amount * (denoised[y * aw + x] - original);
            *sample = store(value);
        }
    }
    Ok(())
}

/// Radius, in samples of one 2 × 2 position (two photosites each), of the
/// three box passes `keep_means` averages over: close to a Gaussian of 49
/// photosites' spread. Radii from 6 to 96 were measured (notes:
/// benchmark-results/aid-ai-denoise-shadows-2026-11-11/): a narrower window
/// brings back more of the sensor's noise, a wider one follows the shift
/// less closely where a dark area lies beside a bright one.
const MEANS_RADIUS: usize = 24;
const MEANS_PASSES: usize = 3;

/// Adds to `denoised`, per 2 × 2 position, the local mean of `mosaic` −
/// `denoised` (both `aw` × `ah`, whole 2 × 2 tiles), so the result's local
/// means are the sensor's. Samples at or above white take no part: they
/// are kept as recorded.
fn keep_means(mosaic: &[f32], denoised: &mut [f32], aw: usize, ah: usize) {
    let (pw, ph) = (aw / 2, ah / 2);
    for position in 0..4 {
        let at = |x: usize, y: usize| (2 * y + position / 2) * aw + 2 * x + position % 2;
        let mut shift = vec![0.0f32; pw * ph];
        let mut weight = vec![0.0f32; pw * ph];
        shift
            .par_chunks_mut(pw)
            .zip(weight.par_chunks_mut(pw))
            .enumerate()
            .for_each(|(y, (shift, weight))| {
                for x in 0..pw {
                    let i = at(x, y);
                    if mosaic[i] < 1.0 {
                        shift[x] = mosaic[i] - denoised[i];
                        weight[x] = 1.0;
                    }
                }
            });
        for _ in 0..MEANS_PASSES {
            box_sum(&mut shift, pw, ph, MEANS_RADIUS);
            box_sum(&mut weight, pw, ph, MEANS_RADIUS);
        }
        for y in 0..ph {
            for x in 0..pw {
                let i = at(x, y);
                let w = weight[y * pw + x];
                if mosaic[i] < 1.0 && w > 0.0 {
                    denoised[i] += shift[y * pw + x] / w;
                }
            }
        }
    }
}

/// Replaces each value of `plane` (`width` × `height`) by the sum over the
/// box of `radius` around it, cut at the plane's edges. Sums run in f64 in
/// a fixed order, so the result is the same on every machine.
fn box_sum(plane: &mut [f32], width: usize, height: usize, radius: usize) {
    let mut rows = vec![0.0f32; plane.len()];
    rows.par_chunks_mut(width)
        .zip(plane.par_chunks(width))
        .for_each(|(sums, row)| {
            let mut sum: f64 = row.iter().take(radius + 1).map(|&v| v as f64).sum();
            for x in 0..width {
                sums[x] = sum as f32;
                if x + radius + 1 < width {
                    sum += row[x + radius + 1] as f64;
                }
                if x >= radius {
                    sum -= row[x - radius] as f64;
                }
            }
        });
    let mut sum: Vec<f64> = vec![0.0; width];
    for row in rows.chunks(width).take(radius + 1) {
        for (s, &v) in sum.iter_mut().zip(row) {
            *s += v as f64;
        }
    }
    for y in 0..height {
        for (out, &s) in plane[y * width..(y + 1) * width].iter_mut().zip(&sum) {
            *out = s as f32;
        }
        if y + radius + 1 < height {
            for (s, &v) in sum.iter_mut().zip(&rows[(y + radius + 1) * width..]) {
                *s += v as f64;
            }
        }
        if y >= radius {
            for (s, &v) in sum.iter_mut().zip(&rows[(y - radius) * width..]) {
                *s -= v as f64;
            }
        }
    }
}

/// The colours of the 2 × 2 tile at (`left`, `top`) of a Bayer CFA, or
/// `None` for any other pattern.
pub(crate) fn bayer_pattern(cfa: &rawler::CFA, left: usize, top: usize) -> Option<[u8; 4]> {
    if cfa.width != 2 || cfa.height != 2 || !cfa.is_rgb() {
        return None;
    }
    let pattern = [
        cfa.color_at(top, left) as u8,
        cfa.color_at(top, left + 1) as u8,
        cfa.color_at(top + 1, left) as u8,
        cfa.color_at(top + 1, left + 1) as u8,
    ];
    let mut counts = [0; 3];
    for c in pattern {
        if c > 2 {
            return None;
        }
        counts[c as usize] += 1;
    }
    (counts == [1, 2, 1]).then_some(pattern)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Returns a constant, recording what it was given.
    struct Flat(std::sync::Mutex<Option<(usize, usize, [u8; 4])>>);

    impl MosaicDenoiser for Flat {
        fn denoise(&self, m: &[f32], w: usize, h: usize, p: [u8; 4]) -> Result<Vec<f32>> {
            *self.0.lock().unwrap() = Some((w, h, p));
            Ok(vec![0.5; m.len()])
        }
    }

    #[test]
    fn the_active_area_is_blended_and_white_is_kept() {
        // 6 × 4 samples, active area 4 × 2 at (1, 1); white at sample (2, 1).
        let mut data: Vec<u16> = vec![100; 24];
        data[6 + 2] = 1100;
        let flat = Arc::new(Flat(Default::default()));
        let denoise = MosaicDenoise::new(flat.clone(), 0.5);
        // Black 100, white 1100: level 0 everywhere but the clipped sample.
        apply(
            &denoise,
            &mut data,
            6,
            (1, 1, 4, 2),
            [0, 1, 1, 2],
            |v: u16| (v as f32 - 100.0) / 1000.0,
            |l: f32| (l * 1000.0 + 100.0).round() as u16,
        )
        .unwrap();
        assert_eq!(*flat.0.lock().unwrap(), Some((4, 2, [0, 1, 1, 2])));
        assert!(denoise.was_applied());
        // Halfway to 0.5 → level 0.25 → 350; outside the area untouched.
        assert_eq!(data[6 + 1], 350);
        assert_eq!(data[6 + 2], 1100, "clipped sample kept");
        assert_eq!(data[2 * 6 + 4], 350);
        assert_eq!(data[0], 100);
        assert_eq!(data[6 + 5], 100);
    }

    /// Reports two steps per call and returns a constant.
    struct Stepped;

    impl MosaicDenoiser for Stepped {
        fn denoise(&self, m: &[f32], _: usize, _: usize, _: [u8; 4]) -> Result<Vec<f32>> {
            Ok(vec![0.5; m.len()])
        }
        fn denoise_reporting(
            &self,
            m: &[f32],
            w: usize,
            h: usize,
            p: [u8; 4],
            _: f32,
            progress: &(dyn Fn(usize, usize) + Sync),
        ) -> Result<Vec<f32>> {
            progress(1, 2);
            progress(2, 2);
            self.denoise(m, w, h, p)
        }
    }

    #[test]
    fn a_progress_hook_hears_the_denoiser_and_changes_no_sample() {
        let develop = |denoise: &MosaicDenoise| {
            let mut data: Vec<u16> = vec![100; 24];
            apply(
                denoise,
                &mut data,
                6,
                (1, 1, 4, 2),
                [0, 1, 1, 2],
                |v: u16| (v as f32 - 100.0) / 1000.0,
                |l: f32| (l * 1000.0 + 100.0).round() as u16,
            )
            .unwrap();
            data
        };
        let heard = Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = Arc::clone(&heard);
        let reporting = MosaicDenoise::new(Arc::new(Stepped), 0.5).with_progress(Arc::new(
            move |done, total| sink.lock().unwrap().push((done, total)),
        ));
        let quiet = MosaicDenoise::new(Arc::new(Stepped), 0.5);
        assert_eq!(develop(&reporting), develop(&quiet));
        assert_eq!(*heard.lock().unwrap(), [(1, 2), (2, 2)]);
    }

    /// The samples darkened by a different amount per 2 × 2 position, and
    /// smoothed along each row of that position (a denoiser with a shift).
    struct Darkening([f32; 4]);

    impl MosaicDenoiser for Darkening {
        fn denoise(&self, m: &[f32], w: usize, _: usize, _: [u8; 4]) -> Result<Vec<f32>> {
            Ok((0..m.len())
                .map(|i| {
                    let (x, y) = (i % w, i / w);
                    // Neighbours of the same position; white ones count
                    // as the sample itself.
                    let near = |x: usize| Some(m[y * w + x]).filter(|v| *v < 1.0).unwrap_or(m[i]);
                    let left = near(if x >= 2 { x - 2 } else { x });
                    let right = near(if x + 2 < w { x + 2 } else { x });
                    (left + 2.0 * m[i] + right) / 4.0 - self.0[(y % 2) * 2 + x % 2]
                })
                .collect())
        }
    }

    /// A mosaic of `w` × `h` levels (black 0, white 1): a dark half and a
    /// brighter half with noise, one sample at white.
    fn noisy_halves(w: usize, h: usize) -> Vec<f32> {
        let mut seed = 99u64;
        let mut v: Vec<f32> = (0..w * h)
            .map(|i| {
                seed = seed
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                let noise = (seed >> 33) as f32 / (1u64 << 31) as f32 - 0.5;
                (if i % w < w / 2 { 0.002 } else { 0.05 }) + 0.004 * noise
            })
            .collect();
        v[5 * w + 7] = 1.0;
        v
    }

    fn develop(denoise: &MosaicDenoise, levels: &[f32], w: usize, h: usize) -> Vec<f32> {
        let mut data = levels.to_vec();
        apply(
            denoise,
            &mut data,
            w,
            (0, 0, w, h),
            [0, 1, 1, 2],
            |v: f32| v,
            |l: f32| l,
        )
        .unwrap();
        data
    }

    /// Mean of 2 × 2 position `p` over the columns `x0..x1`.
    fn mean(data: &[f32], w: usize, p: usize, x0: usize, x1: usize) -> f64 {
        let picked: Vec<f64> = data
            .iter()
            .enumerate()
            .filter(|(i, v)| {
                let (x, y) = (i % w, i / w);
                (y % 2) * 2 + x % 2 == p && (x0..x1).contains(&x) && **v < 1.0
            })
            .map(|(_, v)| *v as f64)
            .collect();
        picked.iter().sum::<f64>() / picked.len() as f64
    }

    #[test]
    fn sensor_means_survive_a_denoiser_that_shifts_them() {
        let (w, h) = (640, 480);
        let levels = noisy_halves(w, h);
        let shift = [0.0015, 0.0005, 0.0008, 0.0020];
        let plain = MosaicDenoise::new(Arc::new(Darkening(shift)), 1.0);
        let kept = MosaicDenoise::new(Arc::new(Darkening(shift)), 1.0).with_sensor_means(true);
        let (shifted, anchored) = (
            develop(&plain, &levels, w, h),
            develop(&kept, &levels, w, h),
        );
        for (p, by) in shift.into_iter().enumerate() {
            // Both halves, away from the step between them.
            for (x0, x1) in [(0, w / 4), (3 * w / 4, w)] {
                let sensor = mean(&levels, w, p, x0, x1);
                let without = mean(&shifted, w, p, x0, x1);
                let with = mean(&anchored, w, p, x0, x1);
                assert!(
                    (without - (sensor - by as f64)).abs() < 2e-5,
                    "{p}: {without}"
                );
                assert!((with - sensor).abs() < 2e-5, "{p}: {with} for {sensor}");
            }
        }
        // The noise the denoiser took out stays out: the samples' spread
        // around their mean is the denoiser's, not the sensor's.
        let spread = |data: &[f32]| {
            let m = mean(data, w, 0, 0, w / 4);
            let v: Vec<f64> = (0..h)
                .step_by(2)
                .flat_map(|y| (0..w / 4).step_by(2).map(move |x| y * w + x))
                .map(|i| (data[i] as f64 - m).powi(2))
                .collect();
            (v.iter().sum::<f64>() / v.len() as f64).sqrt()
        };
        assert!(spread(&shifted) < 0.75 * spread(&levels));
        assert!((spread(&anchored) / spread(&shifted) - 1.0).abs() < 0.02);
        // White stays as recorded, and without the option nothing changes.
        assert_eq!(anchored[5 * w + 7], 1.0);
        assert_eq!(shifted[5 * w + 7], 1.0);
    }

    #[test]
    fn the_slider_blends_towards_a_result_with_the_sensor_means() {
        let (w, h) = (320, 240);
        let levels = noisy_halves(w, h);
        let shift = [0.001; 4];
        let at = |amount: f32| {
            let denoise =
                MosaicDenoise::new(Arc::new(Darkening(shift)), amount).with_sensor_means(true);
            develop(&denoise, &levels, w, h)
        };
        let (half, full) = (at(0.5), at(1.0));
        for i in [0, 1, w + 1, 37 * w + 200, 201 * w + 15] {
            let blended = levels[i] + 0.5 * (full[i] - levels[i]);
            assert!((half[i] - blended).abs() < 1e-7, "{i}");
        }
        let sensor = mean(&levels, w, 3, 0, w / 4);
        assert!((mean(&half, w, 3, 0, w / 4) - sensor).abs() < 2e-5);
    }

    #[test]
    fn box_sums_are_cut_at_the_edges() {
        // 5 × 3, radius 1: each value becomes the sum of its 3 × 3 box.
        let mut plane: Vec<f32> = (0..15).map(|v| v as f32).collect();
        box_sum(&mut plane, 5, 3, 1);
        assert_eq!(plane[0], 0.0 + 1.0 + 5.0 + 6.0);
        assert_eq!(plane[7], (1 + 2 + 3 + 6 + 7 + 8 + 11 + 12 + 13) as f32);
        assert_eq!(plane[14], 8.0 + 9.0 + 13.0 + 14.0);
        // A radius beyond the plane sums all of it.
        let mut plane = vec![1.0f32; 15];
        box_sum(&mut plane, 5, 3, 48);
        assert_eq!(plane, vec![15.0; 15]);
    }

    #[test]
    fn only_bayer_patterns_qualify() {
        let rggb = rawler::CFA::new("RGGB");
        assert_eq!(bayer_pattern(&rggb, 0, 0), Some([0, 1, 1, 2]));
        // An odd origin shifts the pattern.
        assert_eq!(bayer_pattern(&rggb, 1, 0), Some([1, 0, 2, 1]));
        let xtrans = rawler::CFA::new("GGRGGBGGBGGRBRGRBGGGBGGRGGRGGBRBGBRG");
        assert_eq!(bayer_pattern(&xtrans, 0, 0), None);
    }
}
