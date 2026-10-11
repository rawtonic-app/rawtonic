//! Lightroom-style colour grading (shadows, midtones, highlights and global
//! wheels with hue, saturation and luminance; blending; balance).
//!
//! The model was fitted to Camera Raw renders (notes: reference/color-grading.md)
//! and acts on display values: every wheel moves Oklab lightness and colour
//! by an amount that depends only on the pixel's display lightness L.
//! It is therefore evaluated here once per recipe (and mask layer) into a
//! small table of (dL, da, db) by L, which the shader interpolates
//! (`apply_grading`); clipping to sRGB there limits strong tints as Camera
//! Raw's are limited.
//!
//! - Tint: weight(x) x capacity(L) x saturation / 50 x hue vector. The
//!   capacity is how far Camera Raw moves colours of that lightness for the
//!   global wheel at saturation 50; the hue vector (every 15 degrees) also
//!   carries the lightness change Camera Raw couples to global and midtone
//!   tints (blue darkens, yellow brightens).
//! - Region weights: shadows and highlights from tables by blending (0, 50,
//!   100), midtones a bump (4·tm·(1 − tm))^p that blending leaves alone,
//!   global 1. x is the lightness after Balance, a logit shift.
//! - Luminance acts like a tone curve per region: lifting is limited by the
//!   headroom to white, lowering by the distance to black.
use serde_json::Value;

pub const GRADING_LUT_SIZE: usize = 32;

/// Capacity knots at L = 0, 0.1, … 0.9, 1.
const CAPACITY: [f32; 11] = [
    0.0, 0.0164, 0.0253, 0.0373, 0.0448, 0.0533, 0.0587, 0.0576, 0.0492, 0.0327, 0.0,
];

/// (dL, da, db) per unit capacity at saturation 50, every 15 degrees of
/// Lightroom hue from 0.
const HUE: [[f32; 3]; 24] = [
    [-0.1144, 1.1597, 0.1996],
    [0.0741, 0.8615, 0.3293],
    [0.2803, 0.5568, 0.4409],
    [0.4942, 0.2372, 0.5641],
    [0.7192, -0.0781, 0.6910],
    [0.5828, -0.3907, 0.6476],
    [0.4453, -0.6817, 0.6067],
    [0.3204, -0.9629, 0.5666],
    [0.1977, -1.2415, 0.5274],
    [0.2011, -1.2270, 0.3628],
    [0.2071, -1.2108, 0.1858],
    [0.2147, -1.1938, 0.0067],
    [0.2232, -1.1801, -0.1797],
    [-0.0122, -0.8714, -0.3220],
    [-0.2531, -0.5715, -0.4686],
    [-0.4788, -0.2712, -0.6162],
    [-0.6983, 0.0269, -0.7612],
    [-0.5573, 0.3402, -0.7013],
    [-0.4059, 0.6440, -0.6408],
    [-0.2447, 0.9449, -0.5777],
    [-0.0731, 1.2377, -0.5113],
    [-0.0844, 1.2238, -0.3212],
    [-0.0947, 1.2057, -0.1369],
    [-0.1050, 1.1836, 0.0394],
];

/// Shadow and highlight tint weights by balance-shifted lightness x = 0,
/// 0.1 … 1, for blending 0, 50 and 100 (linear in blending between).
const SHADOW_WEIGHTS: [[f32; 11]; 3] = [
    [
        1.482, 1.571, 1.662, 1.504, 0.903, 0.279, 0.032, 0.002, 0.02, 0.09, 0.099,
    ],
    [
        1.678, 1.704, 1.578, 1.357, 0.99, 0.564, 0.313, 0.222, 0.15, 0.117, 0.075,
    ],
    [
        1.93, 1.79, 1.772, 1.845, 1.676, 1.399, 1.128, 0.837, 0.513, 0.281, 0.006,
    ],
];
const HIGHLIGHT_WEIGHTS: [[f32; 11]; 3] = [
    [
        0.012, 0.006, 0.0, 0.0, 0.0, 0.0, 0.003, 0.223, 0.781, 1.222, 1.659,
    ],
    [
        0.0, 0.0, 0.034, 0.072, 0.123, 0.189, 0.273, 0.489, 1.198, 1.93, 1.976,
    ],
    [
        0.0, 0.0, 0.061, 0.231, 0.445, 0.738, 1.028, 1.308, 1.652, 1.73, 1.96,
    ],
];

/// Tint: balance as a logit shift per 100, midtone bump (gain, centre,
/// width, power), lightness coupling of shadow and highlight tints per unit
/// colour shift.
struct Tint {
    balance: f32,
    midtones: [f32; 4],
    shadows_lift: f32,
    highlights_lift: f32,
}

const TINT: Tint = Tint {
    balance: 1.861,
    midtones: [1.012, 0.6018, 0.2556, 4.933],
    shadows_lift: 0.0248,
    highlights_lift: -0.0039,
};

/// Luminance: region edges (centre and width at blending 0, 50, 100),
/// balance, midtone bump (centre, width), width factor and centre offsets of
/// the shadow/highlight edges; per region (shadows, midtones, highlights,
/// global) the exponent of |l| / 50, the gain and the headroom exponent,
/// each for raising and lowering.
struct Luminance {
    centre: [f32; 3],
    width: [f32; 3],
    balance: f32,
    midtones: [f32; 2],
    width_factor: f32,
    shadows_offset: f32,
    highlights_offset: f32,
    power: [[f32; 2]; 4],
    gain: [[f32; 2]; 4],
    headroom: [[f32; 2]; 4],
}

/// Global luminance for raising / lowering: gain, a, b, exponent of |l| / 50.
const GLOBAL_LUMINANCE: [[f32; 4]; 2] = [
    [0.1402, 1.164, 0.8213, 1.017],
    [0.1158, 1.1747, 0.6176, 1.0189],
];

const LUMINANCE: Luminance = Luminance {
    centre: [0.6532, 0.6269, 0.6254],
    width: [0.1358, 0.1227, 0.1474],
    balance: 1.6847,
    midtones: [0.6015, 0.0973],
    width_factor: 0.9925,
    shadows_offset: -0.2538,
    highlights_offset: 0.3095,
    power: [
        [0.9825, 1.3914],
        [1.2421, 0.9935],
        [1.0291, 0.8246],
        [1.0276, 1.0244],
    ],
    gain: [
        [0.0791, 0.1236],
        [0.0386, 0.0432],
        [0.3906, 0.13],
        [0.0305, 0.0373],
    ],
    headroom: [
        [0.4028, 0.5496],
        [0.2, 0.2546],
        [0.6194, 0.2],
        [0.2, 0.4364],
    ],
};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Wheel {
    pub hue: f32,
    pub saturation: f32,
    pub luminance: f32,
}

impl Wheel {
    fn from_json(value: &Value) -> Self {
        let get = |key: &str| value[key].as_f64().unwrap_or(0.0) as f32;
        Self {
            hue: get("hue").rem_euclid(360.0),
            saturation: get("saturation").clamp(0.0, 100.0),
            luminance: get("luminance").clamp(-100.0, 100.0),
        }
    }

    fn is_neutral(&self) -> bool {
        self.saturation <= 0.0 && self.luminance == 0.0
    }
}

/// A recipe's `colorGrading` in Lightroom's units (hue 0–360, saturation
/// 0–100, luminance −100…100, blending 0–100, balance −100…100).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Grading {
    pub shadows: Wheel,
    pub midtones: Wheel,
    pub highlights: Wheel,
    pub global: Wheel,
    pub blending: f32,
    pub balance: f32,
}

impl Default for Grading {
    fn default() -> Self {
        Self {
            shadows: Wheel::default(),
            midtones: Wheel::default(),
            highlights: Wheel::default(),
            global: Wheel::default(),
            blending: 50.0,
            balance: 0.0,
        }
    }
}

impl Grading {
    pub fn from_json(value: &Value) -> Self {
        if value.is_null() {
            return Self::default();
        }
        Self {
            shadows: Wheel::from_json(&value["shadows"]),
            midtones: Wheel::from_json(&value["midtones"]),
            highlights: Wheel::from_json(&value["highlights"]),
            global: Wheel::from_json(&value["global"]),
            blending: value["blending"].as_f64().unwrap_or(50.0).clamp(0.0, 100.0) as f32,
            balance: value["balance"]
                .as_f64()
                .unwrap_or(0.0)
                .clamp(-100.0, 100.0) as f32,
        }
    }

    pub fn is_neutral(&self) -> bool {
        [self.shadows, self.midtones, self.highlights, self.global]
            .iter()
            .all(Wheel::is_neutral)
    }
}

fn logistic(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

fn by_blending(values: [f32; 3], blending: f32) -> f32 {
    if blending <= 50.0 {
        values[0] + (values[1] - values[0]) * blending / 50.0
    } else {
        values[2] * (blending - 50.0) / 50.0 + values[1] * (100.0 - blending) / 50.0
    }
}

fn capacity(lightness: f32) -> f32 {
    let x = lightness.clamp(0.0, 1.0) * 10.0;
    let i = (x as usize).min(9);
    let f = x - i as f32;
    CAPACITY[i] * (1.0 - f) + CAPACITY[i + 1] * f
}

fn hue_vector(hue: f32) -> [f32; 3] {
    let x = hue.rem_euclid(360.0) / 15.0;
    let i = (x as usize) % 24;
    let f = x - x.floor();
    let (a, b) = (HUE[i], HUE[(i + 1) % 24]);
    std::array::from_fn(|k| a[k] * (1.0 - f) + b[k] * f)
}

fn table(weights: &[[f32; 11]; 3], blending: f32, x: f32) -> f32 {
    let at = |row: &[f32; 11]| {
        let p = x.clamp(0.0, 1.0) * 10.0;
        let i = (p as usize).min(9);
        let f = p - i as f32;
        row[i] * (1.0 - f) + row[i + 1] * f
    };
    by_blending(
        [at(&weights[0]), at(&weights[1]), at(&weights[2])],
        blending,
    )
}

fn shifted(l: f32, balance: f32, per_100: f32) -> f32 {
    let l = l.clamp(1.0e-4, 1.0 - 1.0e-4);
    logistic((l / (1.0 - l)).ln() + per_100 * balance / 100.0)
}

/// Luminance shift of one region at display lightness `l`. `region`:
/// 0 shadows, 1 midtones, 2 highlights, 3 global.
fn luminance_shift(region: usize, amount: f32, l: f32, grading: &Grading) -> f32 {
    if amount == 0.0 {
        return 0.0;
    }
    if region == 3 {
        // Global luminance is a midtone bump g · L^a · (1 − L)^b (fitted
        // directly to Camera Raw's ±50 / ±100 responses).
        let [g, a, b, q] = GLOBAL_LUMINANCE[usize::from(amount < 0.0)];
        let l = l.clamp(0.0, 1.0);
        return amount.signum() * g * l.powf(a) * (1.0 - l).powf(b) * (amount.abs() / 50.0).powf(q);
    }
    let p = &LUMINANCE;
    let x = shifted(l, grading.balance, p.balance);
    let side = usize::from(amount < 0.0);
    let magnitude = (amount.abs() / 50.0).powf(p.power[region][side]) * amount.signum();
    let centre = by_blending(p.centre, grading.blending);
    let width = by_blending(p.width, grading.blending) * p.width_factor;
    let shape = match region {
        0 => 1.0 - logistic((x - centre - p.shadows_offset) / width),
        1 => {
            let tm = logistic((x - p.midtones[0]) / p.midtones[1]);
            4.0 * tm * (1.0 - tm)
        }
        2 => logistic((x - centre - p.highlights_offset) / width),
        _ => 1.0,
    };
    let headroom = if amount > 0.0 {
        (1.0 - l).max(0.0).powf(p.headroom[region][0])
    } else {
        l.max(0.0).powf(p.headroom[region][1])
    };
    magnitude * p.gain[region][side] * shape * headroom
}

/// Oklab shift (dL, da, db) at display lightness `l`.
pub fn shift(grading: &Grading, l: f32) -> [f32; 3] {
    let t = &TINT;
    let x = shifted(l, grading.balance, t.balance);
    let tm = logistic((x - t.midtones[1]) / t.midtones[2]);
    let weights = [
        table(&SHADOW_WEIGHTS, grading.blending, x),
        t.midtones[0] * (4.0 * tm * (1.0 - tm)).powf(t.midtones[3]),
        table(&HIGHLIGHT_WEIGHTS, grading.blending, x),
        1.0,
    ];
    let lift = [t.shadows_lift, 0.0, t.highlights_lift, 0.0];
    let wheels = [
        grading.shadows,
        grading.midtones,
        grading.highlights,
        grading.global,
    ];
    let cap = capacity(l);
    let mut out = [0.0f32; 3];
    for (region, wheel) in wheels.iter().enumerate() {
        if wheel.saturation > 0.0 {
            let v = hue_vector(wheel.hue);
            let scale = weights[region] * cap * wheel.saturation / 50.0;
            let (da, db) = (v[1] * scale, v[2] * scale);
            out[1] += da;
            out[2] += db;
            // Shadow and highlight tints lift lightness slightly whatever
            // the hue; global and midtone tints carry the hue's own change.
            out[0] += if region == 0 || region == 2 {
                lift[region] * da.hypot(db)
            } else {
                v[0] * scale
            };
        }
        out[0] += luminance_shift(region, wheel.luminance, l, grading);
    }
    out
}

/// The shader's grading table: (dL, da, db, 0) at L = i / (size − 1).
pub fn lut(grading: &Grading) -> [[f32; 4]; GRADING_LUT_SIZE] {
    std::array::from_fn(|i| {
        let [dl, da, db] = shift(grading, i as f32 / (GRADING_LUT_SIZE - 1) as f32);
        [dl, da, db, 0.0]
    })
}

#[cfg(test)]
mod tests {
    /// Writes the grading response as JSON to `$GRADING_RESPONSE`: per
    /// region (shadows, midtones, highlights, global), the shift at
    /// saturation 50 for hues 0–355° in 5° steps and luminance ±50, at
    /// display lightness 0, 0.1 … 1 (for fitting importers).
    #[test]
    #[ignore = "tool: needs GRADING_RESPONSE"]
    fn write_grading_response() {
        let ls: Vec<f32> = (0..=10).map(|i| i as f32 / 10.0).collect();
        let mut regions = Vec::new();
        for region in 0..4 {
            let blending: f32 =
                std::env::var("GRADING_BLENDING").map_or(50.0, |v| v.parse().unwrap());
            let balance: f32 = std::env::var("GRADING_BALANCE").map_or(0.0, |v| v.parse().unwrap());
            let set = |wheel: Wheel| {
                let mut g = Grading {
                    blending,
                    balance,
                    ..Grading::default()
                };
                match region {
                    0 => g.shadows = wheel,
                    1 => g.midtones = wheel,
                    2 => g.highlights = wheel,
                    _ => g.global = wheel,
                }
                g
            };
            let hues: Vec<Vec<[f32; 3]>> = (0..72)
                .map(|h| {
                    let g = set(Wheel {
                        hue: h as f32 * 5.0,
                        saturation: 50.0,
                        luminance: 0.0,
                    });
                    ls.iter().map(|&l| shift(&g, l)).collect()
                })
                .collect();
            let lum: Vec<Vec<f32>> = [-50.0f32, 50.0]
                .iter()
                .map(|&amount| {
                    let g = set(Wheel {
                        hue: 0.0,
                        saturation: 0.0,
                        luminance: amount,
                    });
                    ls.iter().map(|&l| shift(&g, l)[0]).collect()
                })
                .collect();
            regions.push(serde_json::json!({ "hues": hues, "luminance": lum }));
        }
        std::fs::write(
            std::env::var("GRADING_RESPONSE").unwrap(),
            serde_json::to_string(&regions).unwrap(),
        )
        .unwrap();
    }

    use super::*;
    use serde_json::json;

    #[test]
    fn neutral_grading_is_detected() {
        assert!(Grading::from_json(&json!({"blending": 50, "balance": 20})).is_neutral());
        assert!(Grading::from_json(&Value::Null).is_neutral());
        assert!(
            !Grading::from_json(&json!({"shadows": {"hue": 220, "saturation": 10}})).is_neutral()
        );
        assert!(!Grading::from_json(&json!({"global": {"luminance": -5}})).is_neutral());
    }

    fn grading(value: Value) -> Grading {
        Grading::from_json(&value)
    }

    fn chroma(shift: [f32; 3]) -> f32 {
        shift[1].hypot(shift[2])
    }

    #[test]
    fn blending_moves_the_shadow_and_highlight_edges() {
        let shadows = |blending: f32| {
            grading(json!({"shadows": {"hue": 0, "saturation": 50}, "blending": blending}))
        };
        // Blending 0 keeps the shadow tint out of the light tones, 100 lets
        // it reach past the middle (Camera Raw).
        assert!(chroma(shift(&shadows(0.0), 0.7)) < 0.002);
        assert!(chroma(shift(&shadows(100.0), 0.7)) > 0.02);
        assert!(chroma(shift(&shadows(0.0), 0.2)) > 0.03);
        let highlights =
            grading(json!({"highlights": {"hue": 40, "saturation": 50}, "blending": 0}));
        assert!(chroma(shift(&highlights, 0.4)) < 0.002);
        assert!(chroma(shift(&highlights, 0.85)) > 0.02);
    }

    #[test]
    fn balance_extends_the_highlight_tint_downwards() {
        let at = |balance: f32| {
            let g =
                grading(json!({"highlights": {"hue": 40, "saturation": 50}, "balance": balance}));
            chroma(shift(&g, 0.5))
        };
        assert!(at(100.0) > 2.0 * at(0.0));
        assert!(at(-100.0) < 0.5 * at(0.0));
    }

    #[test]
    fn hue_follows_lightrooms_wheel() {
        // Hue 0 tints towards red (+a), 240 towards blue (−b).
        let red = shift(
            &grading(json!({"global": {"hue": 0, "saturation": 50}})),
            0.5,
        );
        assert!(red[1] > 0.04 && red[2].abs() < red[1] * 0.3);
        let blue = shift(
            &grading(json!({"global": {"hue": 240, "saturation": 50}})),
            0.5,
        );
        assert!(blue[2] < -0.03 && blue[0] < 0.0);
    }

    #[test]
    fn luminance_keeps_black_and_white() {
        let up = grading(json!({"global": {"luminance": 100}}));
        let down = grading(json!({"global": {"luminance": -100}}));
        assert!(shift(&up, 0.5)[0] > 0.02);
        assert!(shift(&down, 0.5)[0] < -0.02);
        assert!(shift(&up, 1.0)[0].abs() < 1.0e-4);
        assert!(shift(&down, 0.0)[0].abs() < 1.0e-4);
        // Shadow luminance lifts the deepest tones most.
        let shadows = grading(json!({"shadows": {"luminance": 100}}));
        assert!(shift(&shadows, 0.1)[0] > shift(&shadows, 0.5)[0]);
    }

    #[test]
    fn neutral_grading_has_an_empty_table() {
        assert!(
            lut(&Grading::default())
                .iter()
                .all(|row| row.iter().all(|v| *v == 0.0))
        );
    }
}
