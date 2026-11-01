// SPDX-License-Identifier: AGPL-3.0-or-later
//! Manual lens vignetting (Lightroom's Lens Corrections ▸ Manual ▸ Lens
//! Vignetting, `crs:VignetteAmount`/`VignetteMidpoint`): a radial exposure
//! gain of the whole frame before it is cropped, for lenses without a
//! profile or to correct beyond one. Recipe keys
//! `lensManualVignetteAmount` (−100…+100, 0 = none: brightens the corners
//! when positive) and `lensManualVignetteMidpoint` (0…100, 50: how far from
//! the centre it starts).
//!
//! Our own model, fitted to Lightroom Classic 15.6's output on two scenes ×
//! 11 settings (each render's gain read per pixel from the same scene's
//! exposure series; notes: benchmark-results/lc3-lr-import-2026-11-01):
//! the gain in EV is `s(amount) · e^(C0 + C1·m) · ρ^(Q0 + Q1·m)`, with `m`
//! the midpoint ÷ 100, `ρ` the distance from the frame's centre over half
//! its long side, and `s = ±|amount/100|^P` (× `NEGATIVE` when darkening).
//! Residual 0.037 EV rms; fitted on one scene, 0.04–0.055 EV on the other.

/// The recipe key of the amount (−100…+100).
pub const AMOUNT_KEY: &str = "lensManualVignetteAmount";
/// The recipe key of the midpoint (0…100, default 50).
pub const MIDPOINT_KEY: &str = "lensManualVignetteMidpoint";

const C0: f32 = 0.540;
const C1: f32 = -1.409;
const Q0: f32 = 1.494;
const Q1: f32 = 6.082;
const P: f32 = 1.198;
const NEGATIVE: f32 = 0.942;

/// The gain's shape for `amount` and `midpoint`: (EV at ρ = 1, power of ρ),
/// or `None` for no vignetting.
pub fn shape(amount: f32, midpoint: f32) -> Option<(f32, f32)> {
    let amount = amount.clamp(-100.0, 100.0);
    if !amount.is_finite() || amount.abs() < 1e-3 {
        return None;
    }
    let m = if midpoint.is_finite() {
        midpoint.clamp(0.0, 100.0) / 100.0
    } else {
        0.5
    };
    let strength = (amount.abs() / 100.0).powf(P) * if amount < 0.0 { NEGATIVE } else { 1.0 };
    Some((
        amount.signum() * strength * (C0 + C1 * m).exp(),
        Q0 + Q1 * m,
    ))
}

/// The linear gain at `rho` (distance from the centre over half the long
/// side) for a [`shape`].
#[inline(always)]
pub fn gain(ev_at_edge: f32, power: f32, rho: f32) -> f32 {
    (ev_at_edge * rho.max(0.0).powf(power)).exp2()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_amount_is_no_vignetting() {
        assert!(shape(0.0, 50.0).is_none());
        assert!(shape(f32::NAN, 50.0).is_none());
    }

    #[test]
    fn brightens_corners_when_positive_and_leaves_the_centre() {
        let (ev, power) = shape(100.0, 50.0).unwrap();
        assert!((gain(ev, power, 0.0) - 1.0).abs() < 1e-6);
        assert!(gain(ev, power, 1.0) > 1.7);
        let (ev, power) = shape(-100.0, 50.0).unwrap();
        assert!(gain(ev, power, 1.0) < 0.6);
        // A lower midpoint reaches further in.
        let (low, lp) = shape(100.0, 0.0).unwrap();
        let (high, hp) = shape(100.0, 100.0).unwrap();
        assert!(gain(low, lp, 0.5) > gain(high, hp, 0.5));
    }

    #[test]
    fn matches_the_measured_response() {
        // Lightroom 15.6 (o72, the radius band 0.9–1.0 of half the long
        // side): +100 / midpoint 50 +0.76 EV, −100 −0.66, +100 / 100 +0.63
        // (band 1.0–1.1).
        let ev = |amount: f32, midpoint: f32, rho: f32| {
            let (ev, power) = shape(amount, midpoint).unwrap();
            gain(ev, power, rho).log2()
        };
        for (measured, model) in [
            (0.76, ev(100.0, 50.0, 0.95)),
            (-0.66, ev(-100.0, 50.0, 0.95)),
            (0.63, ev(100.0, 100.0, 1.05)),
        ] {
            assert!((measured - model).abs() < 0.15, "{measured} {model}");
        }
    }
}
