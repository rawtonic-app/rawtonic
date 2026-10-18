// SPDX-License-Identifier: AGPL-3.0-or-later
//! A hook for denoisers that work on the Bayer mosaic, before demosaicing.
//!
//! `develop_raw_image_with` hands the active area to the denoiser as
//! black-subtracted, white-normalised samples (1.0 = the sensor's white;
//! clipped samples can exceed it) and blends the result back by `amount`.
//! Samples at or above white are kept as recorded. Only 2 × 2 RGB Bayer
//! mosaics are passed on; other sensors (X-Trans, linear DNGs, Foveon) and
//! fast developments are left alone.

use std::sync::Arc;

use anyhow::Result;

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
}

/// A denoiser and how much of its result to keep (0 … 1).
#[derive(Clone)]
pub struct MosaicDenoise {
    pub denoiser: Arc<dyn MosaicDenoiser>,
    pub amount: f32,
    /// The noise the denoiser is told about, relative to the photo's
    /// (`MosaicDenoiser::denoise_scaled`); 1 for AI Denoise version 1.
    pub noise_scale: f32,
    /// Set once a development ran the denoiser (it skips non-Bayer sensors).
    pub applied: Arc<std::sync::atomic::AtomicBool>,
}

impl MosaicDenoise {
    pub fn new(denoiser: Arc<dyn MosaicDenoiser>, amount: f32) -> Self {
        Self {
            denoiser,
            amount,
            noise_scale: 1.0,
            applied: Arc::default(),
        }
    }

    pub fn with_noise_scale(mut self, noise_scale: f32) -> Self {
        self.noise_scale = noise_scale;
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
    let denoised = if denoise.noise_scale == 1.0 {
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
