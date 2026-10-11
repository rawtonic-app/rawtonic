//! Lightroom's region tone curve (Highlights, Lights, Darks, Shadows and
//! three split points), measured in Camera Raw (notes: reference/region-curve.md):
//! each region adds a fixed shape whose size depends on its slider (tables
//! at −100, −50, +50, +100, linear in between); the regions add up exactly;
//! the shadow split moves only the Shadows shape, the midtone split Darks
//! and Lights, the highlight split Highlights. Applied per channel on
//! display values before the point curve, like Lightroom's parametric curve
//! before its point curve.
use serde_json::Value;

pub const REGION_LUT_ENTRIES: usize = 256;
const KNOTS: usize = 17;

/// Output minus input (0–1) at x = 0, 1/16 … 1 for the default splits
/// (25 / 50 / 75), per region and slider value.
const SHAPES: [[[f32; KNOTS]; 4]; 4] = [
    // shadows: -100, -50, +50, +100
    [
        [
            0.0000, -0.0611, -0.1153, -0.1293, -0.1063, -0.0739, -0.0398, -0.0110, -0.0004, 0.0000,
            0.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000,
        ],
        [
            0.0000, -0.0484, -0.0640, -0.0576, -0.0453, -0.0312, -0.0169, -0.0048, -0.0002, 0.0000,
            0.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000,
        ],
        [
            0.0000, 0.0331, 0.0455, 0.0491, 0.0429, 0.0305, 0.0165, 0.0045, 0.0002, 0.0000, 0.0000,
            0.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000,
        ],
        [
            0.0000, 0.0675, 0.0990, 0.1138, 0.1035, 0.0735, 0.0392, 0.0104, 0.0003, 0.0000, 0.0000,
            0.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000,
        ],
    ],
    // darks: -100, -50, +50, +100
    [
        [
            0.0000, -0.0616, -0.1247, -0.1817, -0.2284, -0.2462, -0.2381, -0.2211, -0.1970,
            -0.1688, -0.1339, -0.1031, -0.0710, -0.0396, -0.0181, -0.0054, 0.0000,
        ],
        [
            0.0000, -0.0554, -0.0963, -0.1124, -0.1168, -0.1141, -0.1061, -0.0962, -0.0844,
            -0.0717, -0.0566, -0.0438, -0.0303, -0.0171, -0.0080, -0.0025, 0.0000,
        ],
        [
            0.0000, 0.0345, 0.0523, 0.0671, 0.0795, 0.0882, 0.0917, 0.0895, 0.0820, 0.0712, 0.0565,
            0.0433, 0.0296, 0.0164, 0.0075, 0.0023, 0.0000,
        ],
        [
            0.0000, 0.0679, 0.1049, 0.1389, 0.1716, 0.1981, 0.2145, 0.2159, 0.2006, 0.1746, 0.1374,
            0.1041, 0.0702, 0.0381, 0.0173, 0.0052, 0.0000,
        ],
    ],
    // lights: -100, -50, +50, +100
    [
        [
            0.0000, -0.0019, -0.0082, -0.0210, -0.0426, -0.0725, -0.1097, -0.1476, -0.1800,
            -0.2041, -0.2195, -0.2221, -0.2129, -0.1941, -0.1558, -0.1010, 0.0000,
        ],
        [
            0.0000, -0.0008, -0.0035, -0.0089, -0.0179, -0.0303, -0.0461, -0.0628, -0.0780,
            -0.0907, -0.1002, -0.1035, -0.1008, -0.0926, -0.0736, -0.0466, 0.0000,
        ],
        [
            0.0000, 0.0009, 0.0037, 0.0092, 0.0183, 0.0314, 0.0489, 0.0687, 0.0890, 0.1084, 0.1258,
            0.1338, 0.1316, 0.1214, 0.0933, 0.0558, 0.0000,
        ],
        [
            0.0000, 0.0019, 0.0081, 0.0209, 0.0427, 0.0754, 0.1208, 0.1712, 0.2205, 0.2594, 0.2774,
            0.2625, 0.2259, 0.1785, 0.1208, 0.0653, 0.0000,
        ],
    ],
    // highlights: -100, -50, +50, +100
    [
        [
            0.0000, -0.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000, -0.0005, -0.0119,
            -0.0464, -0.0815, -0.1157, -0.1421, -0.1334, -0.0949, 0.0000,
        ],
        [
            0.0000, -0.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000, -0.0002, -0.0052,
            -0.0201, -0.0354, -0.0503, -0.0623, -0.0589, -0.0420, 0.0000,
        ],
        [
            0.0000, -0.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0002, 0.0056,
            0.0215, 0.0380, 0.0544, 0.0684, 0.0654, 0.0466, 0.0000,
        ],
        [
            0.0000, -0.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0005, 0.0130,
            0.0524, 0.0899, 0.1206, 0.1397, 0.1109, 0.0643, 0.0000,
        ],
    ],
];

/// How strongly each region's size follows its width between the splits.
const WIDTH_EXPONENT: [f32; 4] = [0.305, 0.518, 0.699, 0.906];
/// Lights: share of the midtone split's move it follows, and the width
/// exponent when it widens (a lower midtone split).
const LIGHTS_SHIFT: f32 = 0.277;
const LIGHTS_WIDENING_EXPONENT: f32 = 0.973;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RegionCurve {
    /// Shadows, darks, lights, highlights (−100…100).
    pub amounts: [f32; 4],
    /// Shadow, midtone and highlight split (0–100).
    pub splits: [f32; 3],
}

impl RegionCurve {
    pub fn from_json(value: &Value) -> Self {
        let get = |key: &str, default: f64| value[key].as_f64().unwrap_or(default) as f32;
        let mut splits = [
            get("shadowSplit", 25.0).clamp(5.0, 45.0),
            get("midtoneSplit", 50.0).clamp(25.0, 75.0),
            get("highlightSplit", 75.0).clamp(55.0, 95.0),
        ];
        splits[1] = splits[1].clamp(splits[0] + 5.0, splits[2] - 5.0);
        Self {
            amounts: [
                get("shadows", 0.0).clamp(-100.0, 100.0),
                get("darks", 0.0).clamp(-100.0, 100.0),
                get("lights", 0.0).clamp(-100.0, 100.0),
                get("highlights", 0.0).clamp(-100.0, 100.0),
            ],
            splits,
        }
    }

    pub fn is_neutral(&self) -> bool {
        self.amounts.iter().all(|a| *a == 0.0)
    }

    /// Output of the curve for a display value x in 0–1.
    pub fn apply(&self, x: f32) -> f32 {
        let [s1, s2, s3] = self.splits.map(|s| s / 100.0);
        // Where each region's default shape is read: piecewise-linear maps
        // that send the moved split points to their default positions.
        let positions = [
            warp(x, &[(0.0, 0.0), (s1, 0.25), (s2, 0.5), (1.0, 1.0)]),
            warp(x, &[(0.0, 0.0), (s2, 0.5), (1.0, 1.0)]),
            // Lights keeps its upper end and slides with the midtone split
            // (Camera Raw); black stays fixed.
            (x + (0.5 - s2) * LIGHTS_SHIFT * (x / 0.05).min(1.0)).clamp(0.0, 1.0),
            warp(x, &[(0.0, 0.0), (s2, 0.5), (s3, 0.75), (1.0, 1.0)]),
        ];
        // A region made wider by its splits also lifts further (Camera Raw;
        // exponents fitted to the moved-split renders).
        // Widths use only the splits that move a region (Darks and Lights
        // ignore the shadow and highlight split, as in Camera Raw).
        let widths = [s1 / 0.25, s2 / 0.5, (1.0 - s2) / 0.5, (1.0 - s3) / 0.25];
        let mut y = x;
        for region in 0..4 {
            let exponent = if region == 2 && widths[region] > 1.0 {
                LIGHTS_WIDENING_EXPONENT
            } else {
                WIDTH_EXPONENT[region]
            };
            y += shape(region, self.amounts[region], positions[region])
                * widths[region].powf(exponent);
        }
        y.clamp(0.0, 1.0)
    }

    /// The shader's table: 256 values packed four per vec4.
    pub fn lut(&self) -> [[f32; 4]; REGION_LUT_ENTRIES / 4] {
        std::array::from_fn(|i| {
            std::array::from_fn(|k| {
                self.apply((i * 4 + k) as f32 / (REGION_LUT_ENTRIES - 1) as f32)
            })
        })
    }
}

fn warp(x: f32, knots: &[(f32, f32)]) -> f32 {
    for pair in knots.windows(2) {
        let ((x0, y0), (x1, y1)) = (pair[0], pair[1]);
        if x <= x1 {
            return y0 + (y1 - y0) * ((x - x0) / (x1 - x0).max(1.0e-6)).clamp(0.0, 1.0);
        }
    }
    1.0
}

fn table(row: &[f32; KNOTS], x: f32) -> f32 {
    let p = x.clamp(0.0, 1.0) * (KNOTS - 1) as f32;
    let i = (p as usize).min(KNOTS - 2);
    let f = p - i as f32;
    row[i] * (1.0 - f) + row[i + 1] * f
}

fn shape(region: usize, amount: f32, x: f32) -> f32 {
    if amount == 0.0 {
        return 0.0;
    }
    let rows = &SHAPES[region];
    let (half, full) = if amount > 0.0 {
        (&rows[2], &rows[3])
    } else {
        (&rows[1], &rows[0])
    };
    let a = amount.abs();
    if a <= 50.0 {
        table(half, x) * a / 50.0
    } else {
        let h = table(half, x);
        h + (table(full, x) - h) * (a - 50.0) / 50.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn neutral_curve_is_identity() {
        let curve = RegionCurve::from_json(&json!({}));
        assert!(curve.is_neutral());
        for i in 0..=20 {
            let x = i as f32 / 20.0;
            assert!((curve.apply(x) - x).abs() < 1.0e-6);
        }
    }

    #[test]
    fn regions_act_where_lightroom_puts_them() {
        let lift = |key: &str, x: f32| RegionCurve::from_json(&json!({key: 100})).apply(x) - x;
        assert!(lift("shadows", 0.15) > 0.08 && lift("shadows", 0.7).abs() < 1.0e-4);
        assert!(lift("darks", 0.35) > 0.15);
        assert!(lift("lights", 0.6) > 0.2 && lift("lights", 0.1) < 0.01);
        assert!(lift("highlights", 0.85) > 0.1 && lift("highlights", 0.4).abs() < 1.0e-4);
        // Black and white stay.
        for key in ["shadows", "darks", "lights", "highlights"] {
            assert!(lift(key, 0.0).abs() < 1.0e-6 && lift(key, 1.0).abs() < 1.0e-6);
        }
    }

    #[test]
    fn splits_move_only_their_neighbours() {
        let darks = RegionCurve::from_json(&json!({"darks": 50}));
        let moved = RegionCurve::from_json(&json!({"darks": 50, "shadowSplit": 15}));
        assert_eq!(darks.apply(0.3), moved.apply(0.3));
        let low = RegionCurve::from_json(&json!({"lights": 100, "midtoneSplit": 30}));
        let high = RegionCurve::from_json(&json!({"lights": 100, "midtoneSplit": 70}));
        assert!(low.apply(0.45) > high.apply(0.45) + 0.05);
    }
}
