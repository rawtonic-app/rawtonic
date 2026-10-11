// Generated from crates/engine/src/region_curve.rs (Lightroom's region curve as
// measured in Camera Raw), for drawing the curve in Develop. The renderer's
// table comes from the Rust module; keep both in step.
import type { RegionCurve } from "./adjustments";

const KNOTS = 17;
// Region (shadows, darks, lights, highlights) × slider (−100, −50, +50, +100).
const SHAPES: number[][][] = [
  [
    [
      0.0, -0.0611, -0.1153, -0.1293, -0.1063, -0.0739, -0.0398, -0.011,
      -0.0004, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
    ],
    [
      0.0, -0.0484, -0.064, -0.0576, -0.0453, -0.0312, -0.0169, -0.0048,
      -0.0002, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
    ],
    [
      0.0, 0.0331, 0.0455, 0.0491, 0.0429, 0.0305, 0.0165, 0.0045, 0.0002, 0.0,
      0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
    ],
    [
      0.0, 0.0675, 0.099, 0.1138, 0.1035, 0.0735, 0.0392, 0.0104, 0.0003, 0.0,
      0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
    ],
  ],
  [
    [
      0.0, -0.0616, -0.1247, -0.1817, -0.2284, -0.2462, -0.2381, -0.2211,
      -0.197, -0.1688, -0.1339, -0.1031, -0.071, -0.0396, -0.0181, -0.0054, 0.0,
    ],
    [
      0.0, -0.0554, -0.0963, -0.1124, -0.1168, -0.1141, -0.1061, -0.0962,
      -0.0844, -0.0717, -0.0566, -0.0438, -0.0303, -0.0171, -0.008, -0.0025,
      0.0,
    ],
    [
      0.0, 0.0345, 0.0523, 0.0671, 0.0795, 0.0882, 0.0917, 0.0895, 0.082,
      0.0712, 0.0565, 0.0433, 0.0296, 0.0164, 0.0075, 0.0023, 0.0,
    ],
    [
      0.0, 0.0679, 0.1049, 0.1389, 0.1716, 0.1981, 0.2145, 0.2159, 0.2006,
      0.1746, 0.1374, 0.1041, 0.0702, 0.0381, 0.0173, 0.0052, 0.0,
    ],
  ],
  [
    [
      0.0, -0.0019, -0.0082, -0.021, -0.0426, -0.0725, -0.1097, -0.1476, -0.18,
      -0.2041, -0.2195, -0.2221, -0.2129, -0.1941, -0.1558, -0.101, 0.0,
    ],
    [
      0.0, -0.0008, -0.0035, -0.0089, -0.0179, -0.0303, -0.0461, -0.0628,
      -0.078, -0.0907, -0.1002, -0.1035, -0.1008, -0.0926, -0.0736, -0.0466,
      0.0,
    ],
    [
      0.0, 0.0009, 0.0037, 0.0092, 0.0183, 0.0314, 0.0489, 0.0687, 0.089,
      0.1084, 0.1258, 0.1338, 0.1316, 0.1214, 0.0933, 0.0558, 0.0,
    ],
    [
      0.0, 0.0019, 0.0081, 0.0209, 0.0427, 0.0754, 0.1208, 0.1712, 0.2205,
      0.2594, 0.2774, 0.2625, 0.2259, 0.1785, 0.1208, 0.0653, 0.0,
    ],
  ],
  [
    [
      0.0, -0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, -0.0005, -0.0119, -0.0464,
      -0.0815, -0.1157, -0.1421, -0.1334, -0.0949, 0.0,
    ],
    [
      0.0, -0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, -0.0002, -0.0052, -0.0201,
      -0.0354, -0.0503, -0.0623, -0.0589, -0.042, 0.0,
    ],
    [
      0.0, -0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0002, 0.0056, 0.0215, 0.038,
      0.0544, 0.0684, 0.0654, 0.0466, 0.0,
    ],
    [
      0.0, -0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0005, 0.013, 0.0524, 0.0899,
      0.1206, 0.1397, 0.1109, 0.0643, 0.0,
    ],
  ],
];
const WIDTH_EXPONENT = [0.305, 0.518, 0.699, 0.906];
const LIGHTS_SHIFT = 0.277;
const LIGHTS_WIDENING_EXPONENT = 0.973;

const table = (row: number[], x: number) => {
  const p = Math.min(1, Math.max(0, x)) * (KNOTS - 1);
  const i = Math.min(Math.floor(p), KNOTS - 2);
  const f = p - i;
  return row[i] * (1 - f) + row[i + 1] * f;
};

const shape = (region: number, amount: number, x: number) => {
  if (amount === 0) return 0;
  const rows = SHAPES[region];
  const [half, full] = amount > 0 ? [rows[2], rows[3]] : [rows[1], rows[0]];
  const a = Math.abs(amount);
  if (a <= 50) return (table(half, x) * a) / 50;
  const h = table(half, x);
  return h + ((table(full, x) - h) * (a - 50)) / 50;
};

const warp = (x: number, knots: [number, number][]) => {
  for (let k = 1; k < knots.length; k++) {
    const [x0, y0] = knots[k - 1];
    const [x1, y1] = knots[k];
    if (x <= x1)
      return (
        y0 +
        (y1 - y0) * Math.min(1, Math.max(0, (x - x0) / Math.max(x1 - x0, 1e-6)))
      );
  }
  return 1;
};

export const isNeutralRegionCurve = (c: RegionCurve) =>
  c.shadows === 0 && c.darks === 0 && c.lights === 0 && c.highlights === 0;

/** Output of the region curve for a display value x in 0–1 (as region_curve.rs). */
export function regionCurveValue(c: RegionCurve, x: number) {
  const clamp = (v: number, lo: number, hi: number) =>
    Math.min(hi, Math.max(lo, v));
  const s1 = clamp(c.shadowSplit, 5, 45);
  const s3 = clamp(c.highlightSplit, 55, 95);
  const s2 = clamp(clamp(c.midtoneSplit, 25, 75), s1 + 5, s3 - 5) / 100;
  const [l, h] = [s1 / 100, s3 / 100];
  const positions = [
    warp(x, [
      [0, 0],
      [l, 0.25],
      [s2, 0.5],
      [1, 1],
    ]),
    warp(x, [
      [0, 0],
      [s2, 0.5],
      [1, 1],
    ]),
    Math.min(
      1,
      Math.max(0, x + (0.5 - s2) * LIGHTS_SHIFT * Math.min(1, x / 0.05)),
    ),
    warp(x, [
      [0, 0],
      [s2, 0.5],
      [h, 0.75],
      [1, 1],
    ]),
  ];
  const widths = [l / 0.25, s2 / 0.5, (1 - s2) / 0.5, (1 - h) / 0.25];
  const amounts = [c.shadows, c.darks, c.lights, c.highlights];
  let y = x;
  for (let r = 0; r < 4; r++) {
    const exponent =
      r === 2 && widths[r] > 1 ? LIGHTS_WIDENING_EXPONENT : WIDTH_EXPONENT[r];
    y += shape(r, amounts[r], positions[r]) * widths[r] ** exponent;
  }
  return Math.min(1, Math.max(0, y));
}
