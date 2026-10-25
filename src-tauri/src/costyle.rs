//! Capture One styles (`.costyle`, also `.cos` settings files) as Rawtonic
//! presets. By Rawtonic's law there is one tool per job, so a style's
//! settings are translated into Rawtonic's tools as closely as they were
//! measured (notes: reference/capture-one-tools.md, "Style import"):
//!
//! - Color Balance: Capture One stores each wheel as RGB multipliers,
//!   `(1 − 0.3·s·(1 − c(h)))·f` with c(h) the hue at full saturation and
//!   f the lightness factor (1 + L/3 raising, 1 + L/4 lowering). Decoded to
//!   hue, saturation and lightness, then mapped per region and hue to the
//!   Rawtonic grading wheel whose Oklab shift by lightness fits Capture
//!   One's measured shift best.
//! - Levels: `R;G;B;RGB` in 0–1; the midtone (−1…1) as the gamma Capture
//!   One's renders showed.
//! - Tone sliders by the values that reproduce Capture One's measured
//!   change; the unmeasured ones are marked below.
//!
//! - Color Editor: each range as a colour editor range, fitted on Capture
//!   One's renders (`color_editor_ranges`).
//!
//! Not translated: ICC profiles and film curves (Capture One's own
//! rendering, files Rawtonic cannot read), Color Editor entries for all
//! colours or inverted ranges, white balance multipliers (camera
//! specific), noise reduction (different baselines), lens and keystone
//! settings, retouching. `untranslated_settings` names them for the import
//! report, along with approximate skin entries, unsupported bounds/clipped
//! values, unknown/malformed entries and ranges beyond the recipe limits.

use serde_json::{Map, Value, json};
use std::collections::HashMap;
use uuid::Uuid;

use crate::file_management::Preset;

/// Rawtonic grading hue and strength (saturation for Capture One's
/// saturation 1) for Capture One hues 0, 15 … 345, and Capture One's
/// saturation exponent, per region: master, shadow, midtone, highlight.
/// Hue and the shape over hue come from a model of Capture One's response
/// matched to Rawtonic's grading; the strength is scaled to Capture One's
/// measured shifts (three photos, saturation 0.5 at six hues and 1.0 at
/// three: the model was ×1.8–2.2 too weak, by a constant per region).
struct WheelMap {
    hue: [f32; 24],
    strength: [f32; 24],
    exponent: f32,
}

const MASTER: WheelMap = WheelMap {
    hue: [
        25.0, 30.0, 35.0, 45.0, 90.0, 105.0, 120.0, 120.0, 120.0, 120.0, 140.0, 195.0, 200.0,
        210.0, 215.0, 230.0, 270.0, 285.0, 295.0, 300.0, 300.0, 300.0, 315.0, 15.0,
    ],
    strength: [
        72.7, 80.6, 75.2, 58.0, 46.7, 48.2, 48.8, 56.6, 63.4, 57.7, 54.7, 68.0, 68.4, 81.1, 76.5,
        56.1, 52.7, 53.1, 55.4, 60.2, 67.3, 61.2, 56.9, 68.0,
    ],
    exponent: 1.1,
};
const SHADOW: WheelMap = WheelMap {
    hue: [
        15.0, 35.0, 45.0, 55.0, 70.0, 80.0, 90.0, 100.0, 125.0, 145.0, 160.0, 180.0, 190.0, 210.0,
        220.0, 230.0, 240.0, 250.0, 260.0, 275.0, 295.0, 315.0, 335.0, 355.0,
    ],
    strength: [
        82.7, 108.9, 110.7, 97.7, 82.1, 92.7, 97.6, 99.6, 90.0, 88.2, 83.0, 75.5, 79.5, 111.0,
        123.6, 124.4, 114.0, 120.8, 118.8, 104.5, 84.2, 77.9, 75.9, 71.1,
    ],
    exponent: 0.93,
};
const MIDTONE: WheelMap = WheelMap {
    hue: [
        15.0, 25.0, 35.0, 50.0, 85.0, 90.0, 105.0, 115.0, 120.0, 145.0, 165.0, 190.0, 200.0, 205.0,
        215.0, 230.0, 260.0, 270.0, 275.0, 285.0, 300.0, 310.0, 330.0, 350.0,
    ],
    strength: [
        84.1, 90.3, 83.4, 62.6, 54.1, 64.8, 68.3, 72.2, 77.8, 78.2, 74.8, 83.5, 91.6, 97.9, 98.1,
        81.0, 79.5, 84.3, 89.9, 89.5, 81.6, 78.7, 76.5, 71.4,
    ],
    exponent: 1.18,
};
const HIGHLIGHT: WheelMap = WheelMap {
    hue: [
        10.0, 30.0, 45.0, 55.0, 65.0, 75.0, 80.0, 95.0, 115.0, 140.0, 160.0, 175.0, 190.0, 210.0,
        225.0, 235.0, 250.0, 260.0, 265.0, 280.0, 295.0, 315.0, 335.0, 355.0,
    ],
    strength: [
        78.8, 106.2, 117.2, 105.3, 92.6, 98.0, 104.3, 95.0, 80.4, 75.2, 72.7, 67.2, 71.8, 99.6,
        110.7, 105.5, 100.9, 102.7, 105.6, 93.9, 82.7, 77.4, 76.3, 72.5,
    ],
    exponent: 1.08,
};

/// Rawtonic luminance for Capture One lightness ±1 and its exponent,
/// raising then lowering, for shadow, midtone and highlight (Capture One
/// reaches beyond Rawtonic's ±100 from about ±0.6).
const LIGHTNESS: [[(f32, f32); 2]; 3] = [
    [(167.1, 0.854), (153.0, 0.728)],
    [(197.9, 0.96), (138.1, 1.04)],
    [(160.7, 0.82), (127.4, 1.175)],
];

/// Capture One's Color Balance multipliers `r;g;b` as hue (degrees),
/// saturation (0–1) and lightness (−1…1).
fn decode_color_balance(value: &str) -> Option<(f32, f32, f32)> {
    let m: Vec<f32> = value
        .split(';')
        .map(|v| v.trim().parse().ok())
        .collect::<Option<_>>()?;
    let [r, g, b] = <[f32; 3]>::try_from(m).ok()?;
    let f = r.max(g).max(b);
    if !(f.is_finite() && f > 0.0) {
        return None;
    }
    let lightness = if f >= 1.0 {
        (f - 1.0) * 3.0
    } else {
        (f - 1.0) * 4.0
    };
    let n = [r / f, g / f, b / f];
    let saturation = (1.0 - n[0].min(n[1]).min(n[2])) / 0.3;
    if saturation < 1e-3 {
        return Some((0.0, 0.0, lightness.clamp(-1.0, 1.0)));
    }
    // The pure hue colour: one channel 1, one 0.
    let c = n.map(|x| 1.0 - (1.0 - x) / (0.3 * saturation));
    let (max, min) = (c[0].max(c[1]).max(c[2]), c[0].min(c[1]).min(c[2]));
    let d = (max - min).max(1e-6);
    let hue = if max == c[0] {
        ((c[1] - c[2]) / d).rem_euclid(6.0)
    } else if max == c[1] {
        (c[2] - c[0]) / d + 2.0
    } else {
        (c[0] - c[1]) / d + 4.0
    } * 60.0;
    Some((
        hue.rem_euclid(360.0),
        saturation.clamp(0.0, 1.0),
        lightness.clamp(-1.0, 1.0),
    ))
}

/// A Rawtonic grading wheel for one decoded Capture One wheel.
fn wheel(map: &WheelMap, region: Option<usize>, (hue, sat, light): (f32, f32, f32)) -> Value {
    let position = hue.rem_euclid(360.0) / 15.0;
    let i = position as usize % 24;
    let j = (i + 1) % 24;
    let t = position.fract();
    let turn = ((map.hue[j] - map.hue[i] + 540.0) % 360.0) - 180.0;
    let rt_hue = (map.hue[i] + t * turn).rem_euclid(360.0);
    let strength = map.strength[i] + t * (map.strength[j] - map.strength[i]);
    let saturation = (strength * sat.powf(map.exponent)).clamp(0.0, 100.0);
    let luminance = region.map_or(0.0, |r| {
        let (k, p) = LIGHTNESS[r][usize::from(light < 0.0)];
        (light.signum() * k * light.abs().powf(p)).clamp(-100.0, 100.0)
    });
    json!({
        "hue": rt_hue.round(),
        "saturation": saturation.round(),
        "luminance": luminance.round(),
    })
}

/// Sets a non-zero value, clamped and rounded to two decimals.
fn put(adjustments: &mut Map<String, Value>, key: &str, value: f64, min: f64, max: f64) {
    if value != 0.0 {
        let rounded = (value.clamp(min, max) * 100.0).round() / 100.0;
        adjustments.insert(key.to_string(), json!(rounded));
    }
}

fn numbers(value: &str, separator: char) -> Option<Vec<f64>> {
    value
        .split(separator)
        .map(|v| v.trim().parse().ok())
        .collect()
}

/// Capture One's level midtone (−1…1) as a Photoshop gamma, fitted to its
/// renders: +1 ≈ 2.04, +0.5 ≈ 1.72, −0.5 ≈ 0.59, −1 ≈ 0.48.
fn midtone_gamma(m: f64) -> f64 {
    (m.signum() * 0.72 * m.abs().powf(0.42)).exp()
}

fn levels(settings: &HashMap<String, String>) -> Option<Value> {
    let get = |key: &str| settings.get(key).and_then(|v| numbers(v, ';'));
    let (shadow, highlight, midtone, target_shadow, target_highlight) = (
        get("Shadow"),
        get("Highlight"),
        get("Midtone"),
        get("TargetShadow"),
        get("TargetHighlight"),
    );
    if [
        &shadow,
        &highlight,
        &midtone,
        &target_shadow,
        &target_highlight,
    ]
    .iter()
    .all(|v| v.is_none())
    {
        return None;
    }
    let at = |values: &Option<Vec<f64>>, i: usize, default: f64| {
        values
            .as_ref()
            .and_then(|v| v.get(i).copied())
            .unwrap_or(default)
    };
    let mut out = Map::new();
    // Stored as R;G;B;RGB.
    for (name, i) in [("red", 0), ("green", 1), ("blue", 2), ("rgb", 3)] {
        let level = json!({
            "inBlack": (at(&shadow, i, 0.0) * 255.0).round(),
            "inWhite": (at(&highlight, i, 1.0) * 255.0).round(),
            "gamma": (midtone_gamma(at(&midtone, i, 0.0)) * 1000.0).round() / 1000.0,
            "outBlack": (at(&target_shadow, i, 0.0) * 255.0).round(),
            "outWhite": (at(&target_highlight, i, 1.0) * 255.0).round(),
        });
        out.insert(name.to_string(), level);
    }
    let identity = out.values().all(|l| {
        l["inBlack"] == json!(0.0)
            && l["inWhite"] == json!(255.0)
            && l["gamma"] == json!(1.0)
            && l["outBlack"] == json!(0.0)
            && l["outWhite"] == json!(255.0)
    });
    (!identity).then_some(Value::Object(out))
}

/// `x,y;x,y…` in 0–1 as Rawtonic curve points in 0–255.
fn curve(value: &str) -> Option<Value> {
    let points: Vec<Value> = value
        .split(';')
        .map(|p| {
            let xy = numbers(p, ',')?;
            (xy.len() == 2).then(|| {
                json!({
                    "x": (xy[0].clamp(0.0, 1.0) * 255.0).round(),
                    "y": (xy[1].clamp(0.0, 1.0) * 255.0).round(),
                })
            })
        })
        .collect::<Option<_>>()?;
    let identity = points.len() == 2
        && points[0] == json!({"x": 0.0, "y": 0.0})
        && points[1] == json!({"x": 255.0, "y": 255.0});
    (points.len() >= 2 && !identity).then_some(Value::Array(points))
}

/// Linear sRGB to Oklab (L, chroma, hue in degrees).
fn srgb_to_oklch(rgb: [f64; 3]) -> (f64, f64, f64) {
    let lin = rgb.map(|c| {
        let c = c.clamp(0.0, 1.0);
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    });
    let lms = [
        0.4122214708 * lin[0] + 0.5363325363 * lin[1] + 0.0514459929 * lin[2],
        0.2119034982 * lin[0] + 0.6806995451 * lin[1] + 0.1073969566 * lin[2],
        0.0883024619 * lin[0] + 0.2817188376 * lin[1] + 0.6299787005 * lin[2],
    ]
    .map(f64::cbrt);
    let l = 0.2104542553 * lms[0] + 0.7936177850 * lms[1] - 0.0040720468 * lms[2];
    let a = 1.9779984951 * lms[0] - 2.4285922050 * lms[1] + 0.4505937099 * lms[2];
    let b = 0.0259040371 * lms[0] + 0.7827717662 * lms[1] - 0.8086757660 * lms[2];
    (l, a.hypot(b), b.atan2(a).to_degrees().rem_euclid(360.0))
}

/// An sRGB colour (0–1) with its HSV hue turned by `degrees`.
fn turn_hsv_hue(rgb: [f64; 3], degrees: f64) -> [f64; 3] {
    let max = rgb[0].max(rgb[1]).max(rgb[2]);
    let min = rgb[0].min(rgb[1]).min(rgb[2]);
    let delta = max - min;
    if delta <= 0.0 {
        return rgb;
    }
    let hue = if max == rgb[0] {
        ((rgb[1] - rgb[2]) / delta).rem_euclid(6.0)
    } else if max == rgb[1] {
        (rgb[2] - rgb[0]) / delta + 2.0
    } else {
        (rgb[0] - rgb[1]) / delta + 4.0
    };
    let h = (hue + degrees / 60.0).rem_euclid(6.0);
    let x = delta * (1.0 - (h % 2.0 - 1.0).abs());
    let (r, g, b) = match h as u32 {
        0 => (delta, x, 0.0),
        1 => (x, delta, 0.0),
        2 => (0.0, delta, x),
        3 => (0.0, x, delta),
        4 => (x, 0.0, delta),
        _ => (delta, 0.0, x),
    };
    [r + min, g + min, b + min]
}

fn hue_difference(a: f64, b: f64) -> f64 {
    (a - b + 540.0).rem_euclid(360.0) - 180.0
}

/// Capture One's Color Editor (`ColorCorrections`, one entry of 18 numbers
/// per range, `;`-separated) as colour editor ranges, and whether some of
/// it could not be translated. Decoded and fitted on test styles rendered by
/// Capture One (notes: reference/capture-one-color-editor.md): 0 one range
/// (1) or all colours (0), 1 on, 3/4/5 hue/saturation/lightness shift in
/// the editor's units, 6–8 the range's colour (sRGB 0–255), 9/10 its hue
/// extent below/above that colour (HSV degrees), 11 the lowest saturation it
/// takes (−100 all), 13 smoothness, 15 inverted. Rawtonic's ranges are
/// symmetric windows in Oklab hue (at most eight); all-colour and inverted
/// entries are not translated. List 2 is a skin entry: fields 14/16/17 are
/// H/S/L uniformity. Its picked colour is the target; both selection and
/// uniformity are approximate and reported as a partial Colour Editor import.
fn color_editor_ranges(value: &str) -> (Vec<Value>, bool) {
    let mut ranges = Vec::new();
    let mut untranslated = false;
    let mut skin_seen = false;
    for entry in value.split(';').map(str::trim).filter(|s| !s.is_empty()) {
        let Some(f) =
            numbers(entry, ',').filter(|v| v.len() == 18 && v.iter().all(|n| n.is_finite()))
        else {
            untranslated = true;
            continue;
        };
        if f[1] == 0.0 {
            continue;
        }
        let skin = f[2] == 2.0;
        if !skin && f[2] != 1.0 {
            untranslated = true;
            continue;
        }
        let uniformity = skin && [f[14], f[16], f[17]].iter().any(|u| *u != 0.0);
        if f[3] == 0.0 && f[4] == 0.0 && f[5] == 0.0 && !uniformity {
            continue;
        }
        if f[0] != 1.0
            || f[15] != 0.0
            || ranges.len() >= 8
            || (skin && skin_seen)
            || f[6..9].iter().any(|c| !(0.0..=255.0).contains(c))
        {
            untranslated = true;
            continue;
        }
        // Skin selection and uniformity are approximate (also without
        // uniformity): the chroma band now follows the picked target,
        // whereas the Advanced shape fit used CE_CHROMA. Name the tool in
        // the partial-import report. The
        // range model cannot express an upper saturation bound, nor keep
        // asymmetric hue edges and the skin's picked target independently.
        untranslated |= skin
            || f[12] != 100.0
            || (skin && (f[9] + f[10]).abs() > 1e-6)
            || !(0.0..=50.0).contains(&f[13])
            || !(-100.0..=100.0).contains(&f[11])
            || (skin
                && [f[14], f[16], f[17]]
                    .iter()
                    .any(|u| !(0.0..=100.0).contains(u)));
        // Capture One turns hues as an HSV turn of the range's colour at
        // 60 % of its saturation and 80 % of its value would (the pixels a
        // range takes are rarely its pure colour), measured in Oklab.
        let pure = [f[6] / 255.0, f[7] / 255.0, f[8] / 255.0];
        let max = pure[0].max(pure[1]).max(pure[2]);
        let rgb = pure.map(|c| 0.8 * (max - 0.6 * (max - c)));
        let (_, _, hue) = srgb_to_oklch(rgb);
        let below = hue_difference(srgb_to_oklch(turn_hsv_hue(rgb, f[9])).2, hue);
        let above = hue_difference(srgb_to_oklch(turn_hsv_hue(rgb, f[10])).2, hue);
        let turn = hue_difference(srgb_to_oklch(turn_hsv_hue(rgb, f[3])).2, hue);
        let saturation = if f[4] > 0.0 {
            CE_SAT_UP * (f[4] / 100.0).powf(CE_SAT_POWER) * 100.0
        } else {
            CE_SAT_DOWN * f[4]
        };
        let lightness = f[5]
            * if f[5] > 0.0 {
                CE_LIGHT_UP
            } else {
                CE_LIGHT_DOWN
            };
        let round = |x: f64| (x * 1000.0).round() / 1000.0;
        let (picked_l, picked_c, picked_h) = srgb_to_oklch(pure);
        untranslated |= (2.0 * CE_HUE_SCALE * turn).abs() > 100.0
            || saturation.abs() > 100.0
            || lightness.abs() > 100.0;
        let mut range = json!({
            "id": Uuid::new_v4().to_string(),
            "hue": round((hue + 0.5 * (below + above)).rem_euclid(360.0)),
            "chroma": CE_CHROMA,
            "lightness": 0.5,
            "hueRange": round((CE_WIDTH_SCALE * 0.5 * (above - below) + CE_WIDTH_OFFSET).clamp(5.0, 90.0)),
            "chromaRange": round((100.0 - CE_CHROMA_FLOOR * (f[11] + 100.0)).clamp(0.0, 100.0)),
            "lightnessRange": 100,
            "smoothness": round((CE_SMOOTH_BASE + CE_SMOOTH_SCALE * f[13]).clamp(0.0, 100.0)),
            "hueShift": round((2.0 * CE_HUE_SCALE * turn).clamp(-100.0, 100.0)),
            "saturation": round(saturation.clamp(-100.0, 100.0)),
            "lightnessShift": round(lightness.clamp(-100.0, 100.0)),
            "hueUniformity": 0,
            "saturationUniformity": 0,
            "lightnessUniformity": 0,
        });
        if skin {
            range["kind"] = json!("skin");
            // Unlike an Advanced range, the centre is also the uniformity
            // target: retain the picked colour, not a typical-pixel proxy.
            range["hue"] = json!(round(picked_h));
            range["chroma"] = json!((picked_c * 1e6).round() / 1e6);
            range["lightness"] = json!((picked_l * 1e6).round() / 1e6);
            for (key, field, scale) in [
                ("hueUniformity", 14, SKIN_UNIFORMITY_H),
                ("saturationUniformity", 16, SKIN_UNIFORMITY_S),
                ("lightnessUniformity", 17, SKIN_UNIFORMITY_L),
            ] {
                range[key] = json!(round(f[field].clamp(0.0, 100.0) * scale));
            }
            skin_seen = true;
        }
        ranges.push(range);
    }
    (ranges, untranslated)
}

// The Color Editor mapping, fitted on Capture One's renders of 22 test
// styles (three photos; notes: reference/capture-one-color-editor.md).
const CE_WIDTH_SCALE: f64 = 0.941;
const CE_WIDTH_OFFSET: f64 = 11.61;
const CE_SMOOTH_BASE: f64 = 35.05;
const CE_SMOOTH_SCALE: f64 = 0.719;
const CE_HUE_SCALE: f64 = 0.958;
const CE_SAT_UP: f64 = 0.705;
const CE_SAT_POWER: f64 = 0.743;
const CE_SAT_DOWN: f64 = 1.151;
const CE_CHROMA: f64 = 0.12;
const CE_CHROMA_FLOOR: f64 = 0.883;
const CE_LIGHT_UP: f64 = 16.65;
const CE_LIGHT_DOWN: f64 = 15.37;

// Our unit conversion, measured on own headless renders against C1's
// change from its neutral render: three portraits, separate H/S/L at 50,
// equal-photo normalized squared Oklab residual on hand-marked skin.
// Fourth portrait and strengths 75/100 excluded from the conversion fit.
// Approximate, not parity: notes reference/capture-one-color-editor.md and
// benchmark-results/cx11-skin-import-2026-10-25/ (includes held-out error).
const SKIN_UNIFORMITY_H: f64 = 0.59;
const SKIN_UNIFORMITY_S: f64 = 0.09;
const SKIN_UNIFORMITY_L: f64 = 0.13;

/// Reads the `<E K="…" V="…"/>` entries; later entries win (a `.cos` file
/// lists defaults before the variant's values).
fn entries(content: &str) -> HashMap<String, String> {
    let re = regex::regex!(r#"<E\s+K="([^"]+)"\s+V="([^"]*)""#);
    re.captures_iter(content)
        .map(|c| (c[1].to_string(), c[2].replace("&amp;", "&")))
        .collect()
}

pub fn convert_costyle_to_preset(content: &str, fallback_name: &str) -> Result<Preset, String> {
    if !content.contains("<SL") && !content.contains("<DL") {
        return Err("Not a Capture One style".to_string());
    }
    let s = entries(content);
    let num = |key: &str| s.get(key).and_then(|v| v.trim().parse::<f64>().ok());
    let mut adjustments = Map::new();

    // Tone and presence, as Rawtonic values that reproduce Capture One's
    // measured change by lightness (three photos, notes: reference/
    // capture-one-tools.md): Highlight ×0.5 raising / ×0.56 lowering (the
    // older HighlightRecovery recovers, i.e. lowers), Shadow ×2.5,
    // White ×0.69 / ×0.44, Black ×0.65 / ×0.79, Contrast ×2.25 / ×0.9,
    // Brightness ÷43.5, Saturation ×0.73, Dehaze ×0.78; Exposure 1:1.
    let signed = |v: f64, up: f64, down: f64| v * if v > 0.0 { up } else { down };
    if let Some(v) = num("Exposure") {
        put(&mut adjustments, "exposure", v, -5.0, 5.0);
    }
    if let Some(v) = num("Contrast") {
        put(
            &mut adjustments,
            "contrast",
            signed(v, 2.25, 0.9),
            -100.0,
            100.0,
        );
    }
    if let Some(v) = num("Brightness") {
        put(&mut adjustments, "brightness", v / 43.5, -5.0, 5.0);
    }
    if let Some(v) = num("Saturation") {
        put(&mut adjustments, "saturation", 0.73 * v, -100.0, 100.0);
    }
    let highlights = num("HighlightRecoveryEx").map_or(0.0, |v| signed(v, 0.5, 0.56))
        - 0.56 * num("HighlightRecovery").unwrap_or(0.0);
    put(&mut adjustments, "highlights", highlights, -100.0, 100.0);
    if let Some(v) = num("ShadowRecovery") {
        put(&mut adjustments, "shadows", 2.5 * v, -100.0, 100.0);
    }
    if let Some(v) = num("WhiteRecovery") {
        put(
            &mut adjustments,
            "whites",
            signed(v, 0.69, 0.44),
            -100.0,
            100.0,
        );
    }
    if let Some(v) = num("BlackRecovery") {
        put(
            &mut adjustments,
            "blacks",
            signed(v, 0.65, 0.79),
            -100.0,
            100.0,
        );
    }
    if let Some(v) = num("DehazeAmount") {
        put(&mut adjustments, "dehaze", 0.78 * v, -100.0, 100.0);
    }
    // Clarity and Structure change local contrast, which these lightness
    // shifts do not see; their ratios come from the band contrast measured
    // in notes: reference/tone-controls.md: Clarity ±50 moved medium contrast
    // ×0.98/×1.09 in Capture One and ×0.95/×1.21 at Rawtonic ±50,
    // Structure ×0.71/×1.84 against ×0.74/×1.49.
    if let Some(v) = num("Clarity") {
        put(
            &mut adjustments,
            "clarity",
            signed(v, 0.5, 0.4),
            -100.0,
            100.0,
        );
    }
    if let Some(v) = num("ClarityStructure") {
        put(
            &mut adjustments,
            "structure",
            signed(v, 1.5, 1.0),
            -100.0,
            100.0,
        );
    }
    // Unmeasured scales: vignetting in EV, grain size, sharpening (Capture One's
    // default 160 / 0.8 px ≈ Lightroom's 40 / 1.0), moiré pattern 1–20 and
    // purple fringing 0–100.
    if let Some(v) = s
        .get("Vignetting")
        .and_then(|v| v.split('|').next()?.trim().parse::<f64>().ok())
    {
        put(&mut adjustments, "vignetteAmount", 25.0 * v, -100.0, 100.0);
    }
    if let Some(impact) = num("FilmGrainAmount") {
        let kind = num("FilmGrainType").map_or(0, |t| t.round() as i64);
        // Impact onto Amount by the grain's measured strength in the
        // midtones (8-bit deviation on a grey chart at Impact 50 against
        // the mapped structure at Amount 50, notes: reference/grain.md).
        let strength = match kind {
            1 => 0.81,
            2 => 1.24,
            3 => 1.02,
            4 => 1.03,
            5 => 1.08,
            _ => 0.52,
        };
        let v = impact * strength;
        put(&mut adjustments, "grainAmount", v, 0.0, 100.0);
        if v > 0.0 {
            let size = num("FilmGrainGranularity").unwrap_or(50.0);
            put(&mut adjustments, "grainSize", size, 0.0, 100.0);
            // Capture One's grain types onto the nearest grain structure by
            // the measured character (notes: reference/grain.md): Fine is
            // even across the tones; Silver Rich, Soft and Cubic are
            // midtone-weighted rounded grain; Tabular has bright specks;
            // Harsh is coarse and hard-edged. FilmGrainType 0-5 in that
            // order (0 Fine is also Capture One's default).
            let structure = match kind {
                1..=3 => "soft",
                4 => "speckled",
                5 => "gritty",
                _ => "even",
            };
            adjustments.insert("grainStructure".into(), json!(structure));
            adjustments.insert(
                rawtonic_engine::image_processing::GRAIN_VERSION_KEY.into(),
                json!(rawtonic_engine::image_processing::GRAIN_VERSION),
            );
        }
    }
    if let Some(v) = num("UsmAmount") {
        adjustments.insert(
            "sharpness".into(),
            json!((v / 4.0).clamp(0.0, 150.0).round()),
        );
        if let Some(r) = num("UsmRadius") {
            put(&mut adjustments, "sharpenRadius", r, 0.5, 3.0);
        }
    }
    if let Some(m) = s.get("Moire").and_then(|v| numbers(v, ';'))
        && m.len() == 2
        && m[0] > 0.0
    {
        put(&mut adjustments, "moireAmount", m[0], 0.0, 100.0);
        put(
            &mut adjustments,
            "moirePattern",
            (m[1] - 1.0) / 19.0 * 100.0,
            0.0,
            100.0,
        );
    }
    if let Some(v) = num("PurpleFringingAmount") {
        put(&mut adjustments, "defringePurpleAmount", v / 5.0, 0.0, 20.0);
    }
    // Color Balance → one colour grading.
    let mut grading = Map::new();
    for (key, region, map, index) in [
        ("ColorBalance", "global", &MASTER, None),
        ("ColorBalanceShadow", "shadows", &SHADOW, Some(0)),
        ("ColorBalanceMidtone", "midtones", &MIDTONE, Some(1)),
        ("ColorBalanceHighlight", "highlights", &HIGHLIGHT, Some(2)),
    ] {
        if let Some(decoded) = s.get(key).and_then(|v| decode_color_balance(v)) {
            let w = wheel(map, index, decoded);
            if w["saturation"] != json!(0.0) || w["luminance"] != json!(0.0) {
                grading.insert(region.to_string(), w);
            }
        }
    }

    // Black & White: six sensitivities onto the eight-channel mix (orange
    // and purple between their neighbours); split toning onto the grading's
    // shadow and highlight wheels (hue 1:1, saturation 1:1, unmeasured).
    if num("BwEnabled").is_some_and(|v| v != 0.0) {
        adjustments.insert("blackAndWhite".into(), json!(true));
        let bw = |key: &str| num(key).unwrap_or(0.0).clamp(-100.0, 100.0);
        let (r, y, g, c, b, m) = (
            bw("BwRed"),
            bw("BwYellow"),
            bw("BwGreen"),
            bw("BwCyan"),
            bw("BwBlue"),
            bw("BwMagenta"),
        );
        let mix = [
            ("red", r),
            ("orange", (r + y) / 2.0),
            ("yellow", y),
            ("green", g),
            ("aqua", c),
            ("blue", b),
            ("purple", (b + m) / 2.0),
            ("magenta", m),
        ];
        if mix.iter().any(|(_, v)| *v != 0.0) {
            let mix: Map<String, Value> = mix
                .iter()
                .map(|(k, v)| (k.to_string(), json!(v.round())))
                .collect();
            adjustments.insert("grayMixer".into(), Value::Object(mix));
        }
        for (hue, sat, region) in [
            ("BwDarkHue", "BwDarkSaturation", "shadows"),
            ("BwLightHue", "BwLightSaturation", "highlights"),
        ] {
            let saturation = num(sat).unwrap_or(0.0);
            if saturation > 0.0 {
                let luminance = grading
                    .get(region)
                    .and_then(|w| w["luminance"].as_f64())
                    .unwrap_or(0.0);
                grading.insert(
                    region.to_string(),
                    json!({
                        "hue": num(hue).unwrap_or(0.0).rem_euclid(360.0).round(),
                        "saturation": saturation.clamp(0.0, 100.0).round(),
                        "luminance": luminance,
                    }),
                );
            }
        }
    }
    if !grading.is_empty() {
        adjustments.insert("colorGrading".into(), Value::Object(grading));
    }

    if let Some(levels) = levels(&s) {
        adjustments.insert("levels".into(), levels);
    }
    // Capture One's RGB curve is Rawtonic's RGB curve and its Luma curve,
    // which keeps saturation, the Luminance curve. Capture One applies its
    // curves to gamma-1.8 values (notes: reference/point-curve.md), so they
    // come over close, not exactly (`untranslated_settings` says so).
    let mut curves = Map::new();
    for (key, channel) in [
        ("GradationCurve", "luma"),
        ("GradationCurveY", "luminance"),
        ("GradationCurveRed", "red"),
        ("GradationCurveGreen", "green"),
        ("GradationCurveBlue", "blue"),
    ] {
        if let Some(points) = s.get(key).and_then(|v| curve(v)) {
            curves.insert(channel.into(), points);
        }
    }
    if !curves.is_empty() {
        let mut full = serde_json::to_value(default_curves()).unwrap_or(json!({}));
        for (k, v) in curves {
            full[k] = v;
        }
        adjustments.insert("curves".into(), full);
        adjustments.insert("curveMode".into(), json!("point"));
    }

    let mut color_editor_untranslated = false;
    if let Some(value) = s.get("ColorCorrections") {
        let (ranges, untranslated) = color_editor_ranges(value);
        color_editor_untranslated = untranslated;
        if !ranges.is_empty() {
            adjustments.insert("colorRanges".into(), Value::Array(ranges));
        }
    }

    let skipped: Vec<&str> = ["ICCProfile", "FilmCurve", "WhiteBalance"]
        .into_iter()
        .filter(|k| s.get(*k).is_some_and(|v| !v.is_empty()))
        .chain(color_editor_untranslated.then_some("ColorCorrections"))
        .collect();
    if !skipped.is_empty() {
        log::info!("Capture One style import: not translated {skipped:?}");
    }
    if adjustments.is_empty() && !skipped.is_empty() {
        return Err(format!(
            "Its look lives in {} — Capture One's own rendering, which Rawtonic does not read",
            skipped.join(", ")
        ));
    }

    let mut name = s
        .get("Name")
        .filter(|n| !n.trim().is_empty())
        .cloned()
        .unwrap_or_else(|| fallback_name.to_string());
    // Part of such a style's look stays behind; say so where it is chosen.
    if skipped
        .iter()
        .any(|k| *k == "ICCProfile" || *k == "FilmCurve")
    {
        name.push_str(" (without its ICC look)");
    }
    Ok(Preset {
        id: Uuid::new_v4().to_string(),
        name,
        adjustments: Value::Object(adjustments),
        include_masks: Some(false),
        include_crop_transform: Some(false),
        preset_type: Some("style".to_string()),
        tool_keys: None,
    })
}

fn default_curves() -> Value {
    let identity = json!([{"x": 0, "y": 0}, {"x": 255, "y": 255}]);
    json!({
        "luma": identity,
        "red": identity,
        "green": identity,
        "blue": identity,
    })
}

/// Settings in a style that `convert_costyle_to_preset` cannot fully translate
/// (see the module comment), as codes the import report names: `profile`,
/// `filmCurve`, `colorEditor`, `whiteBalance`, `noiseReduction`, `lens`,
/// and `curves` for curves that come over only approximately (measured: a
/// few levels off Capture One's renders). `colorEditor` also covers approximate
/// skin ranges, unsupported bounds/clipped values, unknown/malformed entries,
/// inverted/all-colour ranges and ranges beyond the recipe limits.
/// Only settings that change something, or cannot be interpreted, are listed, so a `.cos` file's
/// defaults (ICC `EMBEDDED`, an `Auto` film curve, white balance `1;1;1`,
/// zero amounts) report nothing.
pub fn untranslated_settings(content: &str) -> Vec<&'static str> {
    let s = entries(content);
    let nonzero = |key: &str| {
        s.get(key).is_some_and(|v| {
            v.split([';', ','])
                .any(|n| n.trim().parse::<f64>().is_ok_and(|n| n != 0.0))
        })
    };
    // An image's settings file (`.cos`) holds every setting of that image,
    // so its camera's own profile, white balance and default noise
    // reduction are no part of a look; a style holds only what it sets.
    let settings_file = content.contains("<IMG>") || content.contains("<AL");
    let mut out = Vec::new();
    if s.get("ICCProfile").is_some_and(|v| {
        let v = v.trim();
        !v.is_empty() && v != "EMBEDDED" && !v.to_ascii_lowercase().contains("generic")
    }) {
        out.push("profile");
    }
    if s.get("FilmCurve")
        .is_some_and(|v| !v.trim().is_empty() && !v.contains("Auto"))
    {
        out.push("filmCurve");
    }
    // Approximate skin ranges and unsupported, clipped, malformed, unknown
    // or excess colour ranges (see color_editor_ranges).
    if s.get("ColorCorrections")
        .is_some_and(|v| color_editor_ranges(v).1)
    {
        out.push("colorEditor");
    }
    if !settings_file
        && s.get("WhiteBalance").is_some_and(|v| {
            v.split(';').any(|n| {
                n.trim()
                    .parse::<f64>()
                    .is_ok_and(|n| (n - 1.0).abs() > 1e-6)
            })
        })
    {
        out.push("whiteBalance");
    }
    // Capture One's defaults: luminance, colour and detail 50, surface 0.
    if [
        ("NrAmount", 50.0),
        ("CnrAmount", 50.0),
        ("NoiseDetailsAmount", 50.0),
        ("SurfaceSmoothnessAmount", 0.0),
    ]
    .iter()
    .any(|(k, default)| {
        s.get(*k)
            .and_then(|v| v.trim().parse::<f64>().ok())
            .is_some_and(|v| v != *default)
    }) {
        out.push("noiseReduction");
    }
    if [
        "GradationCurve",
        "GradationCurveY",
        "GradationCurveRed",
        "GradationCurveGreen",
        "GradationCurveBlue",
    ]
    .iter()
    .any(|k| s.get(*k).is_some_and(|v| curve(v).is_some()))
    {
        out.push("curves");
    }
    if [
        "GeometryKeystoneTiltX",
        "GeometryKeystoneTiltY",
        "LensShift",
        "LensTilt",
        "LensLccLightFalloffAmount",
    ]
    .iter()
    .any(|k| nonzero(k))
    {
        out.push("lens");
    }
    out
}

#[cfg(test)]
mod tests {
    /// Converts every style and settings file listed in `COSTYLE_LIST` (one
    /// path per line) and prints what came over and what did not; checks
    /// that nothing fails on real files. `cargo test -p rawtonic-app --lib
    /// converts_real_styles -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn converts_real_styles() {
        let list = std::fs::read_to_string(std::env::var("COSTYLE_LIST").unwrap()).unwrap();
        let (mut ok, mut failed, mut with_ranges) = (0, 0, 0);
        let mut reports = std::collections::BTreeMap::<String, usize>::new();
        for path in list.lines().filter(|l| !l.is_empty()) {
            let Ok(content) = std::fs::read_to_string(path) else {
                continue;
            };
            match super::convert_costyle_to_preset(&content, "x") {
                Ok(preset) => {
                    ok += 1;
                    if preset.adjustments.get("colorRanges").is_some() {
                        with_ranges += 1;
                    }
                }
                Err(_) => failed += 1,
            }
            for code in super::untranslated_settings(&content) {
                *reports.entry(code.to_string()).or_default() += 1;
            }
        }
        println!(
            "converted {ok}, refused {failed}, with colour ranges {with_ranges}, untranslated {reports:?}"
        );
    }

    #[test]
    fn imports_skin_uniformity_without_amount_shifts() {
        let style = r#"<SL><E K="ColorCorrections" V="1,1,2,0,0,0,180,120,90,-25,35,-100,100,15,40,0,60,80"/></SL>"#;
        let a = super::convert_costyle_to_preset(style, "skin")
            .unwrap()
            .adjustments;
        let r = &a["colorRanges"][0];
        assert_eq!(r["kind"], json!("skin"));
        let (l, c, h) = super::srgb_to_oklch([180.0 / 255.0, 120.0 / 255.0, 90.0 / 255.0]);
        for (key, expected) in [("hue", h), ("chroma", c), ("lightness", l)] {
            assert!((r[key].as_f64().unwrap() - expected).abs() < 0.001, "{key}");
        }
        for (key, expected) in [
            ("hueUniformity", 23.6),
            ("saturationUniformity", 5.4),
            ("lightnessUniformity", 10.4),
        ] {
            assert!((r[key].as_f64().unwrap() - expected).abs() < 0.001, "{key}");
        }
        assert_eq!(
            super::untranslated_settings(style),
            ["colorEditor"],
            "approximation must be visible"
        );
    }

    #[test]
    fn reports_unsupported_skin_fields_and_keeps_supported_amounts() {
        // Upper saturation bound is not represented by the range model.
        let style = r#"<SL><E K="ColorCorrections" V="1,1,2,3,-10,1,180,120,90,-30,30,-100,60,15,0,0,0,0"/></SL>"#;
        let a = super::convert_costyle_to_preset(style, "skin")
            .unwrap()
            .adjustments;
        assert_eq!(a["colorRanges"][0]["kind"], json!("skin"));
        let r = &a["colorRanges"][0];
        assert!(r["hueShift"].as_f64().unwrap() > 0.0);
        assert_eq!(r["saturation"], json!(-11.51));
        assert_eq!(r["lightnessShift"], json!(16.65));
        assert_eq!(r["chromaRange"], json!(100.0));
        assert_eq!(r["lightnessRange"], json!(100));
        assert_eq!(r["smoothness"], json!(45.835));
        assert_eq!(super::untranslated_settings(style), ["colorEditor"]);
        assert_eq!(
            super::untranslated_settings(&style.replace(",-100,60,", ",-100,100,")),
            ["colorEditor"],
            "amount-only skin selection is approximate too"
        );
        let advanced = style.replace("1,1,2,", "1,1,1,");
        assert_eq!(super::untranslated_settings(&advanced), ["colorEditor"]);
        assert!(
            super::untranslated_settings(&advanced.replace(",-100,60,", ",-100,100,")).is_empty()
        );
        let advanced = advanced.replace(",-100,60,", ",-100,100,");
        for partial in [
            advanced.replace(",100,15,", ",100,60,"), // smoothness outside the decoded range
            advanced.replace(",-100,100,", ",-101,100,"), // saturation floor clipped
            advanced.replace(",3,-10,1,", ",3,-10,100,"), // lightness shift clipped
        ] {
            assert_eq!(super::untranslated_settings(&partial), ["colorEditor"]);
        }
    }

    #[test]
    fn skin_entries_respect_enable_and_single_skin_range_limit() {
        let skin = "1,1,2,0,0,0,180,120,90,-30,30,-100,100,15,50,0,0,0";
        let (ranges, report) = super::color_editor_ranges(&format!("{skin};{skin}"));
        assert_eq!(ranges.len(), 1);
        assert!(report, "a second skin range must be reported");
        let (ranges, report) = super::color_editor_ranges(&skin.replacen("1,1,2", "1,0,2", 1));
        assert!(ranges.is_empty());
        assert!(!report);
        let (ranges, report) = super::color_editor_ranges(&skin.replace(",50,0,0,0", ",0,0,0,0"));
        assert!(ranges.is_empty());
        assert!(!report);
        let advanced = skin.replacen("1,1,2", "1,1,1", 1);
        assert!(
            super::color_editor_ranges(&advanced).0.is_empty(),
            "unused Basic/Advanced uniformity fields do nothing"
        );
        let advanced = "1,1,1,5,0,0,180,120,90,-30,30,-100,100,15,0,0,0,0";
        let mixed = format!("{advanced};{skin}");
        let (ranges, report) = super::color_editor_ranges(&mixed);
        assert_eq!(ranges.len(), 2);
        assert!(report, "skin uniformity is approximate");
        assert!(ranges[0].get("kind").is_none());
        assert_eq!(ranges[1]["kind"], json!("skin"));
        let full = format!("{};{skin}", [advanced; 8].join(";"));
        assert_eq!(super::color_editor_ranges(&full).0.len(), 8);
        assert!(super::color_editor_ranges(&full).1);
    }

    #[test]
    fn malformed_or_unknown_active_color_entries_are_reported() {
        for entry in [
            "1,1,2,0,0,0,180,120,90", // truncated
            "1,1,2,0,0,0,180,120,90,-30,30,-100,100,15,NaN,0,0,0",
            "1,1,2,0,0,0,180,120,90,-30,30,-100,100,15,oops,0,0,0",
            "1,1,2,0,0,0,180,120,90,-30,30,-100,100,15,50,0,0,0,99", // future field
            "1,1,3,5,0,0,180,120,90,-30,30,-100,100,15,0,0,0,0",     // unknown list
            "1,1,2,0,0,0,180,120,90,-30,30,-100,100,15,50,1,0,0",    // inverted
            "0,1,2,0,0,0,180,120,90,-30,30,-100,100,15,50,0,0,0",    // all colours
        ] {
            // Keep a supported edit so this tests the partial-import report;
            // a style with only an unsupported look is refused outright.
            let style = format!(
                r#"<SL><E K="Exposure" V="0.1"/><E K="ColorCorrections" V="{entry}"/></SL>"#
            );
            let a = super::convert_costyle_to_preset(&style, "x")
                .unwrap()
                .adjustments;
            assert!(a.get("colorRanges").is_none(), "{entry}");
            assert_eq!(
                super::untranslated_settings(&style),
                ["colorEditor"],
                "{entry}"
            );
        }
    }

    #[test]
    fn translates_color_editor_ranges() {
        // A red range turned and desaturated, a disabled one, an unchanged
        // one and an all-colours entry (not translatable).
        let (ranges, untranslated) = super::color_editor_ranges(
            "1,1,1,10,-50,3,255,0,0,-30,30,-100,100,15,0,0,0,0;\
             1,0,1,10,0,0,0,255,0,-30,30,-100,100,15,0,0,0,0;\
             1,1,1,0,0,0,0,0,255,-30,30,-100,100,15,0,0,0,0;\
             0,1,1,0,30,0,255,0,0,-30,30,-100,100,15,0,0,0,0",
        );
        assert!(untranslated);
        assert_eq!(ranges.len(), 1);
        // The same numbers as the fitting script (notes: scripts/reference/
        // c1_color_editor/fit.py).
        let close = |key: &str, want: f64| {
            let got = ranges[0][key].as_f64().unwrap();
            assert!((got - want).abs() < 0.01, "{key}: {got} vs {want}");
        };
        close("hue", 28.246);
        close("hueRange", 45.656);
        close("smoothness", 45.835);
        close("hueShift", 18.439);
        close("saturation", -57.55);
        close("lightnessShift", 49.953);
        close("chromaRange", 100.0);
        let (skin, _) =
            super::color_editor_ranges("1,1,1,-5,-20,2,230,170,140,-20,20,-100,100,15,0,0,0,0");
        assert!((skin[0]["hue"].as_f64().unwrap() - 51.53).abs() < 0.01);
        assert!((skin[0]["hueRange"].as_f64().unwrap() - 42.663).abs() < 0.01);
        assert!((skin[0]["hueShift"].as_f64().unwrap() + 16.998).abs() < 0.01);
        assert!(super::untranslated_settings(
            r#"<SL><E K="ColorCorrections" V="1,1,1,10,0,0,255,0,0,-30,30,-100,100,15,0,0,0,0"/></SL>"#
        )
        .is_empty());
    }

    #[test]
    fn reports_untranslated_settings_only_when_they_change_something() {
        let defaults = r#"<SL><E K="ICCProfile" V="EMBEDDED"/>
            <E K="FilmCurve" V="FileSystemJpeg-Auto.fcrv"/><E K="WhiteBalance" V="1;1;1"/>
            <E K="NrAmount" V="50"/><E K="GeometryKeystoneAmount" V="100"/>
            <E K="ColorCorrections" V="1,1,1,0,0,0,255,0,168.9,-7.1,7.1,-100,100,15,0,0,0,0;0,1,1,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0"/></SL>"#;
        assert!(super::untranslated_settings(defaults).is_empty());
        // An image's settings file: its camera profile and white balance.
        let image = r#"<IMG><AL><E K="ICCProfile" V="FujiX100V-Generic.icm"/>
            <E K="WhiteBalance" V="1.9;1;1.4"/><E K="CnrAmount" V="50"/></AL></IMG>"#;
        assert!(super::untranslated_settings(image).is_empty());
        let style = r#"<SL><E K="ICCProfile" V="Warm Film Look.icc"/>
            <E K="FilmCurve" V="Film Extra Shadow.fcrv"/><E K="WhiteBalance" V="1.2;1;0.9"/>
            <E K="NrAmount" V="40"/><E K="GeometryKeystoneTiltY" V="-12"/>
            <E K="ColorCorrections" V="1,1,1,10,0,0,255,0,168.9,-7.1,7.1,-100,100,15,0,1,0,0"/></SL>"#;
        assert_eq!(
            super::untranslated_settings(style),
            [
                "profile",
                "filmCurve",
                "colorEditor",
                "whiteBalance",
                "noiseReduction",
                "lens"
            ]
        );
    }

    use super::*;

    /// Writes each style in `$COSTYLE_FILES` (`;`-separated) as a Rawtonic
    /// recipe next to it (`<file>.json`).
    #[test]
    #[ignore = "tool: needs COSTYLE_FILES"]
    fn write_converted_styles() {
        for path in std::env::var("COSTYLE_FILES").unwrap().split(';') {
            let content = std::fs::read_to_string(path).unwrap();
            let preset = convert_costyle_to_preset(&content, "style").unwrap();
            std::fs::write(format!("{path}.json"), preset.adjustments.to_string()).unwrap();
        }
    }

    #[test]
    fn color_balance_multipliers_decode_as_capture_one_wrote_them() {
        // Written by Capture One 16 for known settings (test session).
        let cases = [
            ("1;0.970000028610229;0.939999997615814", (30.0, 0.2, 0.0)),
            (
                "0.909333288669586;1.03333330154419;0.909333288669586",
                (120.0, 0.4, 0.1),
            ),
            (
                "0.864499986171722;0.921500027179718;0.949999988079071",
                (200.0, 0.3, -0.2),
            ),
            (
                "1.10000002384186;1.10000002384186;0.935000061988831",
                (60.0, 0.5, 0.3),
            ),
            ("1;0.85;0.85", (0.0, 0.5, 0.0)),
        ];
        for (stored, (h, s, l)) in cases {
            let (hue, sat, light) = decode_color_balance(stored).unwrap();
            assert!((hue - h).abs() < 0.5, "{stored}: hue {hue}");
            assert!((sat - s).abs() < 0.005, "{stored}: sat {sat}");
            assert!((light - l).abs() < 0.005, "{stored}: lightness {light}");
        }
        assert_eq!(decode_color_balance("1;1;1"), Some((0.0, 0.0, 0.0)));
        assert_eq!(decode_color_balance("1;1"), None);
    }

    #[test]
    fn a_style_becomes_one_rawtonic_preset() {
        let style = r#"<?xml version="1.0" encoding="utf-8"?>
<SL Engine="1300">
 <E K="Name" V="Warm matte"/>
 <E K="Exposure" V="0.37"/>
 <E K="Contrast" V="17"/>
 <E K="HighlightRecoveryEx" V="23"/>
 <E K="ShadowRecovery" V="29"/>
 <E K="Clarity" V="40"/>
 <E K="ColorBalanceShadow" V="0.909333288669586;1.03333330154419;0.909333288669586"/>
 <E K="Highlight" V="0.901960790157318;1;1;0.941176474094391"/>
 <E K="Midtone" V="0;0;0;0.25"/>
 <E K="TargetShadow" V="0;0;0;0.0196078438311815"/>
 <E K="GradationCurve" V="0,0.05;0.5,0.55;1,1"/>
 <E K="ICCProfile" V="Film.icc"/>
</SL>"#;
        let preset = convert_costyle_to_preset(style, "file").unwrap();
        let a = &preset.adjustments;
        assert_eq!(preset.name, "Warm matte (without its ICC look)");
        assert_eq!(a["exposure"], json!(0.37));
        assert_eq!(a["contrast"], json!(38.25));
        assert_eq!(a["highlights"], json!(11.5));
        assert_eq!(a["shadows"], json!(72.5));
        assert_eq!(a["clarity"], json!(20.0));
        // Shadow wheel: green, lifted.
        let shadows = &a["colorGrading"]["shadows"];
        assert!((100.0..=150.0).contains(&shadows["hue"].as_f64().unwrap()));
        assert!(shadows["saturation"].as_f64().unwrap() > 10.0);
        assert!(shadows["luminance"].as_f64().unwrap() > 0.0);
        // Levels: RGB composite is the fourth value.
        assert_eq!(a["levels"]["rgb"]["inWhite"], json!(240.0));
        assert_eq!(a["levels"]["red"]["inWhite"], json!(230.0));
        assert_eq!(a["levels"]["rgb"]["outBlack"], json!(5.0));
        assert!(a["levels"]["rgb"]["gamma"].as_f64().unwrap() > 1.3);
        assert_eq!(a["curves"]["luma"][1], json!({"x": 128.0, "y": 140.0}));
        assert_eq!(a["curves"]["red"].as_array().unwrap().len(), 2);
        assert!(a["curves"].get("luminance").is_none());
    }

    #[test]
    fn the_luma_curve_becomes_the_luminance_curve() {
        // Capture One's Luma curve keeps saturation (measured), as the
        // Luminance curve does; it no longer lands on the RGB curve.
        let style =
            r#"<SL><E K="GradationCurveY" V="0,0;0.251,0.188;0.502,0.502;0.753,0.816;1,1"/></SL>"#;
        let a = convert_costyle_to_preset(style, "luma")
            .unwrap()
            .adjustments;
        assert_eq!(a["curves"]["luminance"].as_array().unwrap().len(), 5);
        assert_eq!(a["curves"]["luminance"][1], json!({"x": 64.0, "y": 48.0}));
        assert_eq!(a["curves"]["luma"].as_array().unwrap().len(), 2);
        assert_eq!(
            rawtonic_engine::point_curve::luminance_points(&a["curves"]).len(),
            5
        );
        // The import report says the curves come over only approximately;
        // neutral curves report nothing.
        assert_eq!(super::untranslated_settings(style), vec!["curves"]);
        assert!(
            super::untranslated_settings(r#"<SL><E K="GradationCurve" V="0,0;1,1"/></SL>"#)
                .is_empty()
        );
    }

    #[test]
    fn black_and_white_maps_six_sensitivities_onto_eight_channels() {
        let style = r#"<SL><E K="BwEnabled" V="1"/><E K="BwRed" V="-40"/>
            <E K="BwYellow" V="20"/><E K="BwBlue" V="30"/><E K="BwMagenta" V="-10"/>
            <E K="BwLightHue" V="40"/><E K="BwLightSaturation" V="25"/></SL>"#;
        let a = convert_costyle_to_preset(style, "bw").unwrap().adjustments;
        assert_eq!(a["blackAndWhite"], json!(true));
        assert_eq!(a["grayMixer"]["orange"], json!(-10.0));
        assert_eq!(a["grayMixer"]["purple"], json!(10.0));
        assert_eq!(a["colorGrading"]["highlights"]["hue"], json!(40.0));
        assert_eq!(convert_costyle_to_preset(style, "bw").unwrap().name, "bw");
    }

    #[test]
    fn grain_sharpening_and_vignetting_come_along() {
        let style = r#"<SL><E K="FilmGrainAmount" V="50"/><E K="FilmGrainGranularity" V="70"/>
            <E K="UsmAmount" V="0"/><E K="Vignetting" V="-1|1|0|1"/><E K="Moire" V="20;8"/></SL>"#;
        let a = convert_costyle_to_preset(style, "g").unwrap().adjustments;
        assert_eq!(a["grainAmount"], json!(26.0));
        assert_eq!(a["grainSize"], json!(70.0));
        assert_eq!(a["sharpness"], json!(0.0));
        assert_eq!(a["vignetteAmount"], json!(-25.0));
        assert_eq!(a["moireAmount"], json!(20.0));
        assert_eq!(a["grainStructure"], json!("even"));
        assert_eq!(
            a["grainVersion"],
            json!(rawtonic_engine::image_processing::GRAIN_VERSION)
        );
    }

    #[test]
    fn grain_types_map_onto_the_nearest_grain_structure() {
        for (kind, expected) in [
            (0, Some("even")),
            (1, Some("soft")),
            (2, Some("soft")),
            (3, Some("soft")),
            (4, Some("speckled")),
            (5, Some("gritty")),
        ] {
            let style = format!(
                r#"<SL><E K="FilmGrainAmount" V="40"/><E K="FilmGrainType" V="{kind}"/></SL>"#
            );
            let a = convert_costyle_to_preset(&style, "g").unwrap().adjustments;
            assert_eq!(a["grainStructure"].as_str(), expected, "type {kind}");
        }
    }

    #[test]
    fn a_look_that_lives_only_in_an_icc_profile_is_refused_with_the_reason() {
        let style = r#"<SL><E K="ICCProfile" V="film.icc"/><E K="Name" V="Film"/></SL>"#;
        let error = convert_costyle_to_preset(style, "f").unwrap_err();
        assert!(error.contains("ICCProfile"), "{error}");
    }

    #[test]
    fn untouched_settings_add_nothing() {
        let style = r#"<SL><E K="Name" V="n"/><E K="ColorBalance" V="1;1;1"/>
            <E K="Highlight" V="1;1;1;1"/><E K="GradationCurve" V="0,0;1,1"/></SL>"#;
        let a = convert_costyle_to_preset(style, "x").unwrap().adjustments;
        assert_eq!(a, json!({}));
        assert!(convert_costyle_to_preset("{}", "x").is_err());
    }
}
