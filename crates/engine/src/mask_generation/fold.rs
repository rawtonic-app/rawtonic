//! How a layer's components combine: each component after the first folds
//! into the coverage so far by its mode (Add, Subtract, Intersect). The one
//! place for both the 8-bit mask (`eight_bit_mask`) and the floating-point
//! one (`precise::combined_coverage`).
//!
//! Two laws, chosen by the layer's `maskVersion`:
//!
//! - before 5: Add = max(a, b), Intersect = min(a, b), Subtract = a − b;
//! - 5 and up (`recipe_versions::mask::SOFT_FOLD`): Add = a + b − a·b ("screen"),
//!   Intersect = a·b, Subtract = a − b, as Camera Raw and Capture One 16.9
//!   combine soft masks (Subtract a − b = Capture One: it cancels exactly
//!   where a mask is subtracted from itself). Measured on their renders, notes:
//!   benchmark-results/mcq1-2026-11-21/REPORT.md; max left a seam where two
//!   soft edges meet, min a wider intersection.
//!
//! Hard (0/1) coverages give the same result under both laws.

use rayon::prelude::*;

use crate::masks::{MaskDefinition, SubMaskMode};
use crate::recipe_versions::MaskVersion;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FoldLaw {
    /// max / a − b / min: layers before `maskVersion` 5.
    Extremes,
    /// screen / a − b / product: `maskVersion` 5 and up.
    Soft,
}

impl FoldLaw {
    pub(super) fn of(def: &MaskDefinition) -> Self {
        if MaskVersion::new(def.mask_version).soft_fold() {
            Self::Soft
        } else {
            Self::Extremes
        }
    }

    /// Folds `sub` into `target` (8-bit coverages, 255 = full).
    pub(super) fn fold_bytes(self, mode: SubMaskMode, target: &mut [u8], sub: &[u8]) {
        let pixels = target.iter_mut().zip(sub);
        match (self, mode) {
            (_, SubMaskMode::Subtractive) => {
                pixels.for_each(|(pixel, sub)| *pixel = pixel.saturating_sub(*sub))
            }
            (Self::Extremes, SubMaskMode::Additive) => {
                pixels.for_each(|(pixel, sub)| *pixel = (*pixel).max(*sub))
            }
            (Self::Extremes, SubMaskMode::Intersect) => {
                pixels.for_each(|(pixel, sub)| *pixel = (*pixel).min(*sub))
            }
            (Self::Soft, SubMaskMode::Additive) => {
                pixels.for_each(|(pixel, sub)| *pixel = screen_byte(*pixel, *sub))
            }
            (Self::Soft, SubMaskMode::Intersect) => {
                pixels.for_each(|(pixel, sub)| *pixel = product_byte(*pixel, *sub))
            }
        }
    }

    /// Folds `sub` into `target` (coverages in 0..=1).
    pub(super) fn fold_coverage(self, mode: SubMaskMode, target: &mut [f32], sub: &[f32]) {
        let pixels = target.par_iter_mut().zip(sub.par_iter());
        match (self, mode) {
            (_, SubMaskMode::Subtractive) => {
                pixels.for_each(|(pixel, sub)| *pixel = (*pixel - *sub).max(0.0))
            }
            (Self::Extremes, SubMaskMode::Additive) => {
                pixels.for_each(|(pixel, sub)| *pixel = pixel.max(*sub))
            }
            (Self::Extremes, SubMaskMode::Intersect) => {
                pixels.for_each(|(pixel, sub)| *pixel = pixel.min(*sub))
            }
            (Self::Soft, SubMaskMode::Additive) => {
                pixels.for_each(|(pixel, sub)| *pixel = *pixel + *sub - *pixel * *sub)
            }
            (Self::Soft, SubMaskMode::Intersect) => pixels.for_each(|(pixel, sub)| *pixel *= *sub),
        }
    }
}

/// round(a · b / 255), exact for every pair of bytes.
fn product_byte(a: u8, b: u8) -> u8 {
    let t = a as u32 * b as u32 + 128;
    ((t + (t >> 8)) >> 8) as u8
}

/// 255 − round((255 − a)(255 − b) / 255): the screen of two byte coverages.
fn screen_byte(a: u8, b: u8) -> u8 {
    255 - product_byte(255 - a, 255 - b)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MODES: [SubMaskMode; 3] = [
        SubMaskMode::Additive,
        SubMaskMode::Subtractive,
        SubMaskMode::Intersect,
    ];

    fn bytes(law: FoldLaw, mode: SubMaskMode, a: u8, b: u8) -> u8 {
        let mut target = [a];
        law.fold_bytes(mode, &mut target, &[b]);
        target[0]
    }

    fn coverage(law: FoldLaw, mode: SubMaskMode, a: f32, b: f32) -> f32 {
        let mut target = [a];
        law.fold_coverage(mode, &mut target, &[b]);
        target[0]
    }

    #[test]
    fn soft_law_is_screen_product_and_difference() {
        let (a, b) = (0.65_f32, 0.7_f32);
        let close = |x: f32, y: f32| assert!((x - y).abs() < 1e-6, "{x} vs {y}");
        close(coverage(FoldLaw::Soft, SubMaskMode::Additive, a, b), 0.895);
        close(coverage(FoldLaw::Soft, SubMaskMode::Intersect, a, b), 0.455);
        close(
            coverage(FoldLaw::Soft, SubMaskMode::Subtractive, b, a),
            0.05,
        );
        close(coverage(FoldLaw::Soft, SubMaskMode::Subtractive, a, b), 0.0);
        // A soft mask subtracted from itself cancels exactly.
        close(coverage(FoldLaw::Soft, SubMaskMode::Subtractive, a, a), 0.0);
    }

    #[test]
    fn extremes_law_is_max_min_and_difference() {
        assert_eq!(
            coverage(FoldLaw::Extremes, SubMaskMode::Additive, 0.65, 0.7),
            0.7
        );
        assert_eq!(
            coverage(FoldLaw::Extremes, SubMaskMode::Intersect, 0.65, 0.7),
            0.65
        );
        assert_eq!(
            bytes(FoldLaw::Extremes, SubMaskMode::Additive, 166, 179),
            179
        );
        assert_eq!(
            bytes(FoldLaw::Extremes, SubMaskMode::Intersect, 166, 179),
            166
        );
        assert_eq!(
            bytes(FoldLaw::Extremes, SubMaskMode::Subtractive, 166, 179),
            0
        );
    }

    #[test]
    fn byte_fold_rounds_the_float_law_for_every_pair() {
        for mode in MODES {
            for a in 0..=255u8 {
                for b in 0..=255u8 {
                    let exact = coverage(FoldLaw::Soft, mode, a as f32 / 255.0, b as f32 / 255.0);
                    let got = bytes(FoldLaw::Soft, mode, a, b) as f32;
                    assert!(
                        (got - exact * 255.0).abs() <= 0.5 + 1e-3,
                        "{mode:?} {a} {b}: {got} vs {}",
                        exact * 255.0
                    );
                }
            }
        }
    }

    #[test]
    fn hard_coverages_fold_alike_under_both_laws() {
        for mode in MODES {
            for (a, b) in [(0u8, 0u8), (0, 255), (255, 0), (255, 255)] {
                assert_eq!(
                    bytes(FoldLaw::Soft, mode, a, b),
                    bytes(FoldLaw::Extremes, mode, a, b)
                );
                let (x, y) = (a as f32 / 255.0, b as f32 / 255.0);
                assert_eq!(
                    coverage(FoldLaw::Soft, mode, x, y),
                    coverage(FoldLaw::Extremes, mode, x, y)
                );
            }
            // Folding with a hard coverage keeps or clears a soft one alike.
            for a in 0..=255u8 {
                for b in [0u8, 255] {
                    assert_eq!(
                        bytes(FoldLaw::Soft, mode, a, b),
                        bytes(FoldLaw::Extremes, mode, a, b),
                        "{mode:?} {a} {b}"
                    );
                }
            }
        }
    }

    #[test]
    fn law_follows_the_layer_version() {
        let layer = |version: Option<u32>| MaskDefinition {
            mask_version: version,
            ..serde_json::from_value(serde_json::json!({
                "id": "l", "name": "l", "visible": true, "invert": false,
                "opacity": 100.0, "adjustments": {}, "subMasks": []
            }))
            .unwrap()
        };
        for version in [None, Some(2), Some(3), Some(4)] {
            assert_eq!(FoldLaw::of(&layer(version)), FoldLaw::Extremes);
        }
        for version in [Some(5), Some(6)] {
            assert_eq!(FoldLaw::of(&layer(version)), FoldLaw::Soft);
        }
    }
}
