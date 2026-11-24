//! B&W Toning (`blackAndWhiteToning`): the tint a black-and-white photo's
//! grey takes by its lightness — a shadows tone and a highlights tone with
//! hue, saturation and density, a Balance and a Paper control. Rawtonic's
//! own model (notes: benchmark-results/spt1-2026-11-24).
//!
//! Both reference editors tone a B&W photo as a function of the grey value
//! alone (Capture One's toning is a gradient map: a table read off a grey
//! ramp reproduces its toned photos to ΔE00 0.05), so the tint is evaluated
//! here once per recipe into a small table of Oklab (dL, da, db) by the
//! grey's Oklab lightness, which the shader adds after the curves
//! (`apply_bw_toning`).
//!
//! - Hue: the Oklab hue of the fully saturated sRGB colour of that hue (the
//!   colour the slider shows), turning with lightness as both references'
//!   tints do (highlights 12°, shadows 22° per unit of lightness).
//! - Highlights: a soft hinge from the Balance point, rising linearly to
//!   white; with Paper 0 it fades out above L 0.86, so white stays white,
//!   with Paper 100 it tints white like a toned paper base.
//! - Shadows: rising linearly from black (black stays black) to 0.69 of the
//!   Balance point, then falling towards white.
//! - Density 0 keeps the grey's lightness; each unit darkens the tone by
//!   that share of its chroma (Capture One's toning: about 80).
//! - Every entry is fitted into the sRGB cube: above it by lowering the
//!   lightness (a tinted white is darker than white), below it by lowering
//!   the chroma; lightness never falls with a lighter grey.
//!
//! The shape constants were fitted on Capture One 16.9.1 and Rawtonic's
//! Lightroom-type colour grading on a grey ramp (three toned settings each).
use rawtonic_recipe::color::BwToning;

pub const BW_TONING_LUT_SIZE: usize = 32;

/// Balance point (Oklab L where the highlights tone starts) at Balance 0,
/// and its travel at ±100.
const PIVOT: f64 = 0.45;
const PIVOT_TRAVEL: f64 = 0.25;
/// Width of the highlights hinge's soft start (share of its range).
const SOFT_START: f64 = 0.12;
/// Where the highlights tone fades towards white at Paper 0.
const FADE_START: f64 = 0.86;
/// The shadows tone's peak as a share of the Balance point, and the power
/// of its fall towards white.
const SHADOW_PEAK: f64 = 0.69;
const SHADOW_FALL: f64 = 1.9;
/// Oklab chroma per unit of saturation at full weight.
const CHROMA_PER_UNIT: f64 = 0.0028;
/// Hue turn in degrees per unit of lightness, and the lightness it turns
/// around (highlights, shadows).
const TURN: [(f64, f64); 2] = [(12.0, 0.75), (22.0, 0.3)];

fn smoothstep(x: f64) -> f64 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

fn srgb_to_linear(c: f64) -> f64 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_oklab([r, g, b]: [f64; 3]) -> [f64; 3] {
    let l = (0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b).cbrt();
    let m = (0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b).cbrt();
    let s = (0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b).cbrt();
    [
        0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s,
        1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s,
        0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s,
    ]
}

fn oklab_to_linear([lightness, a, b]: [f64; 3]) -> [f64; 3] {
    let l = (lightness + 0.3963377774 * a + 0.2158037573 * b).powi(3);
    let m = (lightness - 0.1055613458 * a - 0.0638541728 * b).powi(3);
    let s = (lightness - 0.0894841775 * a - 1.2914855480 * b).powi(3);
    [
        4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s,
        -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s,
        -0.0041960863 * l - 0.7034186147 * m + 1.7076147010 * s,
    ]
}

fn in_srgb(lab: [f64; 3]) -> bool {
    oklab_to_linear(lab)
        .iter()
        .all(|&c| (-1e-6..=1.0 + 1e-6).contains(&c))
}

/// Oklab hue angle (radians) of the fully saturated sRGB colour of `hue`
/// degrees (HSV hue, value and saturation 1).
fn hue_angle(hue: f64) -> f64 {
    let h = hue.rem_euclid(360.0) / 60.0;
    let x = 1.0 - (h % 2.0 - 1.0).abs();
    let rgb = match h as u32 {
        0 => [1.0, x, 0.0],
        1 => [x, 1.0, 0.0],
        2 => [0.0, 1.0, x],
        3 => [0.0, x, 1.0],
        4 => [x, 0.0, 1.0],
        _ => [1.0, 0.0, x],
    };
    let [_, a, b] = linear_to_oklab(rgb.map(srgb_to_linear));
    b.atan2(a)
}

/// Weights of the highlights and shadows tones at Oklab lightness `l`.
fn weights(l: f64, balance: f64, paper: f64) -> (f64, f64) {
    let pivot = (PIVOT + PIVOT_TRAVEL * balance).clamp(0.1, 0.9);
    let x = ((l - pivot) / (1.0 - pivot)).clamp(0.0, 1.0);
    let rise = if x < SOFT_START {
        x * x / (2.0 * SOFT_START)
    } else {
        x - SOFT_START / 2.0
    } / (1.0 - SOFT_START / 2.0);
    let fade = 1.0 - (1.0 - paper) * smoothstep((l - FADE_START) / (1.0 - FADE_START));
    let peak = SHADOW_PEAK * pivot;
    let shadows = if l < peak {
        (l / peak).max(0.0)
    } else {
        ((1.0 - l) / (1.0 - peak)).clamp(0.0, 1.0).powf(SHADOW_FALL)
    };
    (rise * fade, shadows)
}

/// The tone's Oklab colour for a grey of lightness `l`, before the fit into
/// sRGB.
fn toned(toning: &BwToning, l: f64) -> [f64; 3] {
    let amount = |v: f64| v.clamp(0.0, BwToning::AMOUNT_MAX) / BwToning::AMOUNT_MAX;
    let (wh, ws) = weights(
        l,
        toning.balance.clamp(BwToning::BALANCE_MIN, BwToning::BALANCE_MAX) / BwToning::BALANCE_MAX,
        amount(toning.paper),
    );
    let mut out = [l, 0.0, 0.0];
    for (band, weight, (turn, around)) in [
        (&toning.highlights, wh, TURN[0]),
        (&toning.shadows, ws, TURN[1]),
    ] {
        let chroma = weight * band.saturation.clamp(0.0, BwToning::AMOUNT_MAX) * CHROMA_PER_UNIT;
        if chroma <= 0.0 {
            continue;
        }
        let angle = hue_angle(band.hue) + (turn * (l - around)).to_radians();
        out[0] -= amount(band.density) * chroma;
        out[1] += chroma * angle.cos();
        out[2] += chroma * angle.sin();
    }
    out
}

/// The largest share of `lab`'s chroma at its lightness inside sRGB.
fn fit_chroma(lab: [f64; 3]) -> [f64; 3] {
    if in_srgb(lab) {
        return lab;
    }
    let (mut lo, mut hi) = (0.0, 1.0);
    for _ in 0..40 {
        let mid = 0.5 * (lo + hi);
        if in_srgb([lab[0], lab[1] * mid, lab[2] * mid]) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    [lab[0], lab[1] * lo, lab[2] * lo]
}

/// Whether the recipe's toning tints anything.
pub fn is_active(toning: &BwToning) -> bool {
    toning.shadows.saturation > 0.0 || toning.highlights.saturation > 0.0
}

/// The shader's table at Oklab L = i / (size − 1): (dL, da, db, 0).
pub fn lut(toning: &BwToning) -> [[f32; 4]; BW_TONING_LUT_SIZE] {
    let mut colors = [[0.0f64; 3]; BW_TONING_LUT_SIZE];
    for (i, color) in colors.iter_mut().enumerate() {
        let l = i as f64 / (BW_TONING_LUT_SIZE - 1) as f64;
        let mut lab = toned(toning, l);
        // Above the cube (a light tint): give up lightness, keep the tint.
        if !in_srgb(lab) && oklab_to_linear(lab).iter().any(|&c| c > 1.0) {
            for step in 1..=400 {
                let lower = [lab[0] - 0.001 * step as f64, lab[1], lab[2]];
                if in_srgb(lower) {
                    lab = lower;
                    break;
                }
            }
        }
        *color = fit_chroma(lab);
    }
    for i in 1..BW_TONING_LUT_SIZE {
        if colors[i][0] < colors[i - 1][0] {
            colors[i][0] = colors[i - 1][0];
            colors[i] = fit_chroma(colors[i]);
        }
    }
    std::array::from_fn(|i| {
        let l = i as f64 / (BW_TONING_LUT_SIZE - 1) as f64;
        let [lightness, a, b] = colors[i];
        [(lightness - l) as f32, a as f32, b as f32, 0.0]
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rawtonic_recipe::color::ToneBand;

    fn tone(hue: f64, saturation: f64, density: f64) -> ToneBand {
        ToneBand {
            hue,
            saturation,
            density,
        }
    }

    /// Inside sRGB up to the table's f32 rounding.
    fn inside(lab: [f64; 3]) -> bool {
        oklab_to_linear(lab).iter().all(|&c| (-1e-4..=1.0 + 1e-4).contains(&c))
    }

    fn grey_l(i: usize) -> f64 {
        i as f64 / (BW_TONING_LUT_SIZE - 1) as f64
    }

    #[test]
    fn neutral_toning_is_inactive_and_shifts_nothing() {
        let toning = BwToning::default();
        assert!(!is_active(&toning));
        assert!(lut(&toning).iter().flatten().all(|&v| v == 0.0));
        // Hue, density, Balance and Paper alone tint nothing.
        let toning = BwToning {
            shadows: tone(220.0, 0.0, 50.0),
            highlights: tone(40.0, 0.0, 80.0),
            balance: 30.0,
            paper: 100.0,
        };
        assert!(!is_active(&toning));
        assert!(lut(&toning).iter().flatten().all(|&v| v == 0.0));
    }

    #[test]
    fn hue_is_the_slider_colours_hue() {
        // Pure sRGB red, yellow and blue as Oklab hues.
        assert!((hue_angle(0.0).to_degrees() - 29.2).abs() < 0.5);
        assert!((hue_angle(60.0).to_degrees() - 109.8).abs() < 0.5);
        assert!((hue_angle(240.0).to_degrees() + 95.9).abs() < 0.5);
    }

    #[test]
    fn default_toning_keeps_black_white_and_lightness() {
        let toning = BwToning {
            shadows: tone(220.0, 60.0, 0.0),
            highlights: tone(40.0, 60.0, 0.0),
            balance: 0.0,
            paper: 0.0,
        };
        let table = lut(&toning);
        assert_eq!(table[0], [0.0; 4], "black stays black");
        assert_eq!(table[BW_TONING_LUT_SIZE - 1], [0.0; 4], "white stays white");
        for (i, entry) in table.iter().enumerate() {
            // Lightness is kept unless the tint would leave sRGB there.
            let kept = [grey_l(i), entry[1] as f64, entry[2] as f64];
            assert!(
                entry[0].abs() < 1e-6 || (entry[0] < 0.0 && !in_srgb(kept)),
                "lightness kept at {i}: {entry:?}"
            );
            let lab = [grey_l(i) + entry[0] as f64, entry[1] as f64, entry[2] as f64];
            assert!(inside(lab), "inside sRGB at {i}");
        }
        // Warm highlights, cool shadows.
        let mid_light = table[24];
        assert!(mid_light[2] > 0.02, "highlights warm: {mid_light:?}");
        let mid_dark = table[8];
        assert!(mid_dark[2] < -0.02, "shadows cool: {mid_dark:?}");
    }

    #[test]
    fn paper_tints_white_by_giving_up_lightness() {
        let toning = BwToning {
            highlights: tone(40.0, 60.0, 0.0),
            paper: 100.0,
            ..BwToning::default()
        };
        let white = lut(&toning)[BW_TONING_LUT_SIZE - 1];
        assert!(white[1].hypot(white[2]) > 0.08, "white is tinted: {white:?}");
        assert!(white[0] < -0.02, "a tinted white is darker than white: {white:?}");
    }

    #[test]
    fn density_darkens_the_tone() {
        let base = BwToning {
            highlights: tone(40.0, 60.0, 0.0),
            ..BwToning::default()
        };
        let dense = BwToning {
            highlights: tone(40.0, 60.0, 80.0),
            ..base
        };
        let (a, b) = (lut(&base)[22], lut(&dense)[22]);
        let chroma = (b[1] as f64).hypot(b[2] as f64);
        assert!(a[0].abs() < 1e-6);
        assert!((b[0] as f64 + 0.8 * chroma).abs() < 1e-3, "dL = −0.8 C: {b:?}");
    }

    #[test]
    fn lightness_never_falls_with_a_lighter_grey() {
        for paper in [0.0, 50.0, 100.0] {
            for density in [0.0, 100.0] {
                let toning = BwToning {
                    shadows: tone(250.0, 100.0, density),
                    highlights: tone(55.0, 100.0, density),
                    balance: -40.0,
                    paper,
                };
                let table = lut(&toning);
                for i in 1..BW_TONING_LUT_SIZE {
                    let (prev, here) = (grey_l(i - 1) + table[i - 1][0] as f64, grey_l(i) + table[i][0] as f64);
                    assert!(here >= prev - 1e-6, "paper {paper} density {density} at {i}");
                    let lab = [here, table[i][1] as f64, table[i][2] as f64];
                    assert!(inside(lab), "inside sRGB at {i}");
                }
            }
        }
    }

    #[test]
    fn balance_moves_where_the_highlights_tone_starts() {
        let at = |balance| {
            let toning = BwToning {
                highlights: tone(40.0, 60.0, 0.0),
                balance,
                ..BwToning::default()
            };
            let entry = lut(&toning)[15];
            (entry[1] as f64).hypot(entry[2] as f64)
        };
        assert!(at(-100.0) > at(0.0) && at(0.0) > at(100.0));
        assert_eq!(at(100.0), 0.0);
    }
}
