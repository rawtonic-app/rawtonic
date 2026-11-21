/**
 * Target colours for the colour editor: a range's sampled colour (its
 * centre) is moved onto a target colour by solving the range's own hue,
 * saturation and lightness shifts. The model is the shader's
 * `apply_color_ranges` (crates/gpu/src/shaders/shader.wgsl) with the
 * engine's packing (`pack_color_range`, crates/engine/src/image_processing.rs),
 * evaluated at the range centre; nothing new is stored in the recipe.
 *
 * Targets are entered and shown as sRGB (0–255, hex) or CIE L*C*h° (D50,
 * the Lab of colour specifications and of Photoshop's readout); ΔE00 is
 * measured in that Lab.
 */
import type { ColorRange } from './adjustments';

export type Vec3 = [number, number, number];

/** The colour editor's target (`useEditorStore.colorTarget`): CIELAB D50,
 * where it was sampled (null when typed) and the range it was last applied
 * to (photo, mask layer or null for the photo's own ranges, range id). */
export interface ColorTarget {
  lab: Vec3;
  sampledIn: string | null;
  appliedTo: { photo: string; layer: string | null; rangeId: string } | null;
}

// --- sRGB, Oklab, CIELAB (D50) ------------------------------------------

const clamp = (v: number, lo: number, hi: number) => Math.min(hi, Math.max(lo, v));
const decode = (v: number) => {
  const c = Math.abs(v);
  const lin = c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
  return Math.sign(v) * lin;
};
const encode = (v: number) => {
  const c = Math.abs(v);
  const e = c <= 0.0031308 ? c * 12.92 : 1.055 * c ** (1 / 2.4) - 0.055;
  return Math.sign(v) * e;
};

/** Linear sRGB to Oklab, the shader's matrices (signed cube root). */
export function linearToOklab([r, g, b]: Vec3): Vec3 {
  const l = Math.cbrt(0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b);
  const m = Math.cbrt(0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b);
  const s = Math.cbrt(0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b);
  return [
    0.2104542553 * l + 0.793617785 * m - 0.0040720468 * s,
    1.9779984951 * l - 2.428592205 * m + 0.4505937099 * s,
    0.0259040371 * l + 0.7827717662 * m - 0.808675766 * s
  ];
}

export function oklabToLinear([L, a, b]: Vec3): Vec3 {
  const l = (L + 0.3963377774 * a + 0.2158037573 * b) ** 3;
  const m = (L - 0.1055613458 * a - 0.0638541728 * b) ** 3;
  const s = (L - 0.0894841775 * a - 1.291485548 * b) ** 3;
  return [
    4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s,
    -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s,
    -0.0041960863 * l - 0.7034186147 * m + 1.707614701 * s
  ];
}

/** sRGB 0–255 (display encoded, may lie outside 0–255) to linear. */
export const srgb8ToLinear = (rgb: Vec3): Vec3 => rgb.map((v) => decode(v / 255)) as Vec3;
export const linearToSrgb8 = (lin: Vec3): Vec3 => lin.map((v) => encode(v) * 255) as Vec3;

// Linear sRGB (D65) ↔ XYZ D50, Bradford-adapted (as ICC profiles do).
const SRGB_TO_XYZ_D50 = [
  [0.4360747, 0.3850649, 0.1430804],
  [0.2225045, 0.7168786, 0.0606169],
  [0.0139322, 0.0971045, 0.7141733]
];
const XYZ_D50_TO_SRGB = [
  [3.1338561, -1.6168667, -0.4906146],
  [-0.9787684, 1.9161415, 0.033454],
  [0.0719453, -0.2289914, 1.4052427]
];
const D50: Vec3 = [0.96422, 1, 0.82521];
const mul = (m: number[][], v: Vec3): Vec3 => [
  m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
  m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
  m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2]
];
const EPS = 216 / 24389;
const KAPPA = 24389 / 27;

export function linearToLab(lin: Vec3): Vec3 {
  const xyz = mul(SRGB_TO_XYZ_D50, lin);
  const f = xyz.map((v, i) => {
    const t = v / D50[i];
    return t > EPS ? Math.cbrt(t) : (KAPPA * t + 16) / 116;
  });
  return [116 * f[1] - 16, 500 * (f[0] - f[1]), 200 * (f[1] - f[2])];
}

export function labToLinear([L, a, b]: Vec3): Vec3 {
  const fy = (L + 16) / 116;
  const fx = fy + a / 500;
  const fz = fy - b / 200;
  const inv = (f: number) => (f ** 3 > EPS ? f ** 3 : (116 * f - 16) / KAPPA);
  const xyz: Vec3 = [inv(fx) * D50[0], L > KAPPA * EPS ? fy ** 3 : L / KAPPA, inv(fz) * D50[2]];
  return mul(XYZ_D50_TO_SRGB, xyz);
}

export const labToLch = ([L, a, b]: Vec3): Vec3 => [
  L,
  Math.hypot(a, b),
  ((Math.atan2(b, a) * 180) / Math.PI + 360) % 360
];
export const lchToLab = ([L, C, h]: Vec3): Vec3 => [
  L,
  C * Math.cos((h * Math.PI) / 180),
  C * Math.sin((h * Math.PI) / 180)
];

/** Whether a linear colour lies inside the sRGB cube (with rounding slack). */
export const insideSrgb = (lin: Vec3) => lin.every((v) => v >= -1e-4 && v <= 1 + 1e-4);

// Linear sRGB ↔ linear ProPhoto, the shader's matrices: the renderer's
// working gamut is the ProPhoto unit cube (`gamut_unit`).
const SRGB_TO_PROPHOTO = [
  [0.5293458, 0.3300728, 0.1405812],
  [0.0983744, 0.8734611, 0.0281647],
  [0.0168832, 0.1176725, 0.8654443]
];
const PROPHOTO_TO_SRGB = [
  [2.0340763, -0.7273343, -0.3067417],
  [-0.2288135, 1.2317302, -0.0029169],
  [-0.0085698, -0.1532866, 1.1618565]
];
export const linearToProphoto = (lin: Vec3) => mul(SRGB_TO_PROPHOTO, lin);
export const prophotoToLinear = (pro: Vec3) => mul(PROPHOTO_TO_SRGB, pro);

/** Whether a linear sRGB colour lies inside the working gamut (with rounding slack). */
export const insideWorkingGamut = (lin: Vec3) => linearToProphoto(lin).every((v) => v >= -1e-4 && v <= 1 + 1e-4);

/** The shader's `gamut_unit`: a linear sRGB colour clipped to the ProPhoto unit cube. */
export function gamutUnit(lin: Vec3): Vec3 {
  if (lin.every((v) => v >= 0 && v <= 1)) return lin;
  return prophotoToLinear(linearToProphoto(lin).map((v) => clamp(v, 0, 1)) as Vec3);
}

/** The shader's display values (`encode_signed`) of linear sRGB, and back. */
const encodeSigned = (lin: Vec3): Vec3 => lin.map(encode) as Vec3;
const decodeSigned = (d: Vec3): Vec3 => d.map(decode) as Vec3;

/** "#rrggbb" of an sRGB colour (clipped and rounded). */
export function hexOf(rgb: Vec3) {
  return `#${rgb
    .map((v) =>
      Math.round(Math.min(255, Math.max(0, v)))
        .toString(16)
        .padStart(2, '0')
    )
    .join('')}`;
}

/** sRGB 0–255 of "#rgb", "rrggbb", "#rrggbb" (case-insensitive), else null. */
export function parseHex(text: string): Vec3 | null {
  const t = text.trim().replace(/^#/, '');
  const full = /^[0-9a-f]{3}$/i.test(t) ? t.replace(/./g, (c) => c + c) : t;
  if (!/^[0-9a-f]{6}$/i.test(full)) return null;
  return [0, 2, 4].map((i) => parseInt(full.slice(i, i + 2), 16)) as Vec3;
}

/** CIEDE2000 colour difference of two CIELAB colours. */
export function deltaE00(lab1: Vec3, lab2: Vec3): number {
  const [L1, a1, b1] = lab1;
  const [L2, a2, b2] = lab2;
  const rad = Math.PI / 180;
  const C1 = Math.hypot(a1, b1);
  const C2 = Math.hypot(a2, b2);
  const Cm = (C1 + C2) / 2;
  const G = 0.5 * (1 - Math.sqrt(Cm ** 7 / (Cm ** 7 + 25 ** 7)));
  const a1p = (1 + G) * a1;
  const a2p = (1 + G) * a2;
  const C1p = Math.hypot(a1p, b1);
  const C2p = Math.hypot(a2p, b2);
  const hp = (a: number, b: number) => (a === 0 && b === 0 ? 0 : (((Math.atan2(b, a) / rad) % 360) + 360) % 360);
  const h1p = hp(a1p, b1);
  const h2p = hp(a2p, b2);
  const dLp = L2 - L1;
  const dCp = C2p - C1p;
  let dhp = 0;
  if (C1p * C2p !== 0) {
    dhp = h2p - h1p;
    if (dhp > 180) dhp -= 360;
    else if (dhp < -180) dhp += 360;
  }
  const dHp = 2 * Math.sqrt(C1p * C2p) * Math.sin((dhp * rad) / 2);
  const Lpm = (L1 + L2) / 2;
  const Cpm = (C1p + C2p) / 2;
  let hpm = h1p + h2p;
  if (C1p * C2p !== 0) {
    if (Math.abs(h1p - h2p) > 180) hpm += h1p + h2p < 360 ? 360 : -360;
    hpm /= 2;
  }
  const T =
    1 -
    0.17 * Math.cos((hpm - 30) * rad) +
    0.24 * Math.cos(2 * hpm * rad) +
    0.32 * Math.cos((3 * hpm + 6) * rad) -
    0.2 * Math.cos((4 * hpm - 63) * rad);
  const dTheta = 30 * Math.exp(-(((hpm - 275) / 25) ** 2));
  const Rc = 2 * Math.sqrt(Cpm ** 7 / (Cpm ** 7 + 25 ** 7));
  const Sl = 1 + (0.015 * (Lpm - 50) ** 2) / Math.sqrt(20 + (Lpm - 50) ** 2);
  const Sc = 1 + 0.045 * Cpm;
  const Sh = 1 + 0.015 * Cpm * T;
  const Rt = -Math.sin(2 * dTheta * rad) * Rc;
  return Math.sqrt((dLp / Sl) ** 2 + (dCp / Sc) ** 2 + (dHp / Sh) ** 2 + Rt * (dCp / Sc) * (dHp / Sh));
}

// --- The shader's colour ranges -------------------------------------------

const smoothstep = (e0: number, e1: number, x: number) => {
  const t = clamp((x - e0) / (e1 - e0), 0, 1);
  return t * t * (3 - 2 * t);
};
// WGSL's `%` keeps the dividend's sign, as JavaScript's does.
const wrap180 = (d: number) => (((d % 360) + 540) % 360) - 180;

/** ΔE00 up to which a target counts as reached. */
export const TARGET_REACHED = 1;
/** Limits of the range shifts (the sliders and `pack_color_range`). */
export const SHIFT_LIMIT = 100;
/** Degrees of Oklab hue per hue shift step, lightness change per step at L 0.5. */
const HUE_PER_STEP = 0.5;
const LIGHTNESS_PER_STEP = 0.2 / 100;

interface Packed {
  hue: number;
  chroma: number;
  lightness: number;
  lightnessHalf: number;
  hueHalf: number;
  chromaLow: number;
  chromaHigh: number;
  soft: number;
  dh: number;
  sat: number;
  dl: number;
  uh: number;
  us: number;
  ul: number;
}

const num = (v: number | undefined, fallback: number) => (Number.isFinite(v) ? (v as number) : fallback);

/** `pack_color_range`: a range as the shader reads it. */
function pack(range: ColorRange): Packed {
  const chroma = Math.max(num(range.chroma, 0), 0);
  const chromaRange = clamp(num(range.chromaRange, 70), 0, 100) / 100;
  const lightnessRange = clamp(num(range.lightnessRange, 100), 0, 100) / 100;
  return {
    hue: ((num(range.hue, 0) % 360) + 360) % 360,
    chroma,
    lightness: clamp(num(range.lightness, 0.5), 0, 1),
    lightnessHalf: 0.02 + lightnessRange,
    hueHalf: clamp(num(range.hueRange, 25), 5, 90),
    chromaLow: chroma * (1 - chromaRange),
    chromaHigh: chroma * (1 + 2 * chromaRange) + 0.01,
    soft: 0.1 + (0.9 * clamp(num(range.smoothness, 50), 0, 100)) / 100,
    dh: clamp(num(range.hueShift, 0), -100, 100) * HUE_PER_STEP,
    sat: clamp(num(range.saturation, 0), -100, 100) / 100,
    dl: (clamp(num(range.lightnessShift, 0), -100, 100) / 100) * 0.2,
    uh: clamp(num(range.hueUniformity, 0), 0, 100) / 100,
    us: clamp(num(range.saturationUniformity, 0), 0, 100) / 100,
    ul: clamp(num(range.lightnessUniformity, 0), 0, 100) / 100
  };
}

const bandWeight = (x: number, lo: number, hi: number, soft: number) => {
  const feather = 0.5 * soft * Math.max(hi - lo, 0) + 0.004;
  return smoothstep(lo - feather, lo + feather, x) * (1 - smoothstep(hi - feather, hi + feather, x));
};

/** `color_range_weight` of an Oklab colour. */
function weight(p: Packed, L: number, chroma: number, hue: number) {
  const d = Math.abs(wrap180(hue - p.hue));
  const wh = 1 - smoothstep(p.hueHalf * (1 - p.soft), p.hueHalf * (1 + p.soft), d);
  const wc = bandWeight(chroma, p.chromaLow, p.chromaHigh, p.soft);
  const wl =
    p.lightnessHalf < 1 ? bandWeight(L, p.lightness - p.lightnessHalf, p.lightness + p.lightnessHalf, p.soft) : 1;
  return wh * wc * wl * smoothstep(0.004, 0.015, chroma);
}

interface Sums {
  dh: number;
  dc: number;
  dl: number;
  scale: number;
}

function accumulate(ranges: Packed[], L: number, chroma: number, hue: number, skip = -1): Sums {
  const sums = { dh: 0, dc: 0, dl: 0, scale: 1 };
  ranges.forEach((p, i) => {
    if (i === skip) return;
    const w = weight(p, L, chroma, hue);
    if (w <= 0) return;
    sums.dh += w * (p.uh * wrap180(p.hue - hue) + p.dh);
    sums.dc += w * p.us * (p.chroma - chroma);
    sums.dl += w * (p.ul * (p.lightness - L) + p.dl * 2 * L);
    sums.scale *= Math.max(1 + w * p.sat, 0);
  });
  return sums;
}

const toLch = ([L, a, b]: Vec3) => ({
  L,
  chroma: Math.hypot(a, b),
  hue: ((Math.atan2(b, a) * 180) / Math.PI + 360) % 360
});

function finish(L: number, chroma: number, hue: number, s: Sums): Vec3 {
  const c = Math.max(chroma + s.dc, 0) * s.scale;
  const h = ((hue + s.dh) * Math.PI) / 180;
  return [clamp(L + s.dl, 0, 1), c * Math.cos(h), c * Math.sin(h)];
}

/** `apply_color_ranges` on an Oklab colour (the ranges the renderer reads). */
export function applyColorRanges(ranges: ColorRange[], lab: Vec3): Vec3 {
  return applyPacked(ranges.map(pack), lab);
}

function applyPacked(packed: Packed[], lab: Vec3): Vec3 {
  const { L, chroma, hue } = toLch(lab);
  return finish(L, chroma, hue, accumulate(packed, L, chroma, hue));
}

/** Newton's method for `applyColorRanges(ranges, c) = shown` from `start`;
 * null when it does not settle. */
function rangesInverse(ranges: ColorRange[], shown: Vec3, start: Vec3): Vec3 | null {
  let c: Vec3 = [...start];
  const h = 1e-5;
  for (let i = 0; i < 40; i += 1) {
    const out = applyColorRanges(ranges, c);
    const r = [shown[0] - out[0], shown[1] - out[1], shown[2] - out[2]];
    if (Math.hypot(r[0], r[1], r[2]) < 1e-7) return c;
    const J = [0, 1, 2].map((k) => {
      const d = [...c] as Vec3;
      d[k] += h;
      const o = applyColorRanges(ranges, d);
      return [(o[0] - out[0]) / h, (o[1] - out[1]) / h, (o[2] - out[2]) / h];
    }); // J[k] = column k
    const det3 = (a: number[], b: number[], cc: number[]) =>
      a[0] * (b[1] * cc[2] - b[2] * cc[1]) - b[0] * (a[1] * cc[2] - a[2] * cc[1]) + cc[0] * (a[1] * b[2] - a[2] * b[1]);
    const det = det3(J[0], J[1], J[2]);
    if (!Number.isFinite(det) || Math.abs(det) < 1e-12) return null;
    let step = [det3(r, J[1], J[2]) / det, det3(J[0], r, J[2]) / det, det3(J[0], J[1], r) / det];
    // Small steps: the ranges' soft edges bend the map.
    const size = Math.hypot(step[0], step[1], step[2]);
    if (size > 0.05) step = step.map((v) => (v * 0.05) / size);
    c = [c[0] + step[0], c[1] + step[1], c[2] + step[2]];
  }
  return null;
}

/**
 * The colour that `applyColorRanges(ranges, ·)` turns into `shown` (Oklab):
 * what a colour sampled in the finished photo was before the ranges, so a
 * range picked (or picked again) on colours the ranges already move selects
 * them by their own colour, as the renderer does. Several colours can end
 * as `shown` (a colour no range touches, or one a range moved there): the
 * `hints` are tried first (a range picked again: its old colour), then
 * `shown` itself; `shown` when none settles.
 */
export function colorBeforeRanges(ranges: ColorRange[], shown: Vec3, hints: Vec3[] = []): Vec3 {
  if (!ranges.length) return shown;
  for (const start of [...hints, shown]) {
    const before = rangesInverse(ranges, shown, start);
    if (before) return before;
  }
  return shown;
}

/**
 * How much a small error in a sampled colour (the preview's 8-bit steps)
 * grows when `colorBeforeRanges` undoes the ranges at `before`: about 1
 * where the ranges only turn or lift colours, 10 where they took 90 % of
 * the chroma away. RMS gain of the inverse Jacobian; Infinity when singular.
 */
export function inverseGain(ranges: ColorRange[], before: Vec3): number {
  const h = 1e-5;
  const out = applyColorRanges(ranges, before);
  const J = [0, 1, 2].map((k) => {
    const d = [...before] as Vec3;
    d[k] += h;
    const o = applyColorRanges(ranges, d);
    return [(o[0] - out[0]) / h, (o[1] - out[1]) / h, (o[2] - out[2]) / h];
  });
  // Rows of J (J[k] holds column k).
  const m = [0, 1, 2].map((i) => [J[0][i], J[1][i], J[2][i]]);
  const det =
    m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1]) -
    m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0]) +
    m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
  if (!Number.isFinite(det) || Math.abs(det) < 1e-12) return Infinity;
  const cof = (r: number, c: number) => {
    const rows = [0, 1, 2].filter((i) => i !== r);
    const cols = [0, 1, 2].filter((i) => i !== c);
    return m[rows[0]][cols[0]] * m[rows[1]][cols[1]] - m[rows[0]][cols[1]] * m[rows[1]][cols[0]];
  };
  let sum = 0;
  for (let r = 0; r < 3; r += 1) for (let c = 0; c < 3; c += 1) sum += (cof(r, c) / det) ** 2;
  return Math.sqrt(sum / 3);
}

/** Oklab of a range's sampled colour. */
export const rangeCentre = (range: ColorRange): Vec3 => {
  const h = (range.hue * Math.PI) / 180;
  return [range.lightness, range.chroma * Math.cos(h), range.chroma * Math.sin(h)];
};

/**
 * What the renderer's probe saw at a pick (`sample_color_range_input`): the
 * colour entering the edited ranges in each pixel of the picker's block
 * around the point, developed at full resolution as the export develops it
 * (Oklab of the unclipped working colour: before the photo's ranges, or for
 * a mask layer after the photo's ranges and the layers below it) and the
 * layer's mask influence there (1 for the photo's own ranges).
 */
export interface RangeInput {
  pixels: Vec3[];
  influence: number[];
  /** The block the renderer read (x, y, w, h, full-resolution px of the
   * shown photograph): the target's later measurements read the same one. */
  block?: [number, number, number, number] | null;
}

/** A range picked in the photo: the normalised point, the range's colour
 * the pick set (Oklab) and the renderer's sample there. */
export interface ColorRangePick {
  point: [number, number];
  centre: Vec3;
  /** The framing the point belongs to (utils/colorRanges `geometryKeyOf`). */
  geometry: string;
  /** Null while the renderer has no sample there (a hidden layer). */
  input: RangeInput | null;
}

/** The input a range stands for without a probe: its sampled colour at full strength. */
export const centreInput = (range: ColorRange): RangeInput => ({ pixels: [rangeCentre(range)], influence: [1] });

/** The pickers' mean of Oklab colours: display values averaged, then converted. */
export function meanOklab(pixels: Vec3[]): Vec3 {
  const sum: Vec3 = [0, 0, 0];
  for (const c of pixels) {
    const d = encodeSigned(gamutUnit(oklabToLinear(c)));
    for (let k = 0; k < 3; k += 1) sum[k] += d[k] / pixels.length;
  }
  return linearToOklab(gamutUnit(decodeSigned(sum)));
}

const meanOf = (values: number[]) => values.reduce((a, b) => a + b, 0) / Math.max(values.length, 1);

/** CIELAB D50 of what the renderer shows at the pick with `packed` ranges. */
function renderedPacked(packed: Packed[], input: RangeInput): Vec3 {
  const sum: Vec3 = [0, 0, 0];
  const n = input.pixels.length;
  input.pixels.forEach((c, i) => {
    const m = input.influence[i] ?? 1;
    // Shader `main`: the ranges' result in display values (`linear_to_display`),
    // a layer's mixed in by its mask influence (none below 0.001).
    const before = encodeSigned(gamutUnit(oklabToLinear(c)));
    const after = packed.length ? encodeSigned(gamutUnit(oklabToLinear(applyPacked(packed, c)))) : before;
    const mix = m >= 1 ? 1 : m > 0.001 ? m : 0;
    for (let k = 0; k < 3; k += 1) sum[k] += (before[k] + (after[k] - before[k]) * mix) / n;
  });
  return linearToLab(gamutUnit(decodeSigned(sum)));
}

/**
 * CIELAB D50 of the colour the renderer leaves at the pick: each pixel of
 * `input` through `ranges` (its layer's mix by the mask influence), in the
 * working gamut (an output space maps it into its own), averaged as the
 * pickers average. Tools after the colour editor are not included.
 */
export const renderedLab = (ranges: ColorRange[], input: RangeInput): Vec3 => renderedPacked(ranges.map(pack), input);

export interface TargetSolve {
  /** The range's new shifts (whole slider steps). */
  shifts: Pick<ColorRange, 'hueShift' | 'saturation' | 'lightnessShift'>;
  /** CIELAB D50 of the colour at the pick after the solve (`renderedLab`), and its ΔE00 to the target. */
  resultLab: Vec3;
  deltaE: number;
  /** Which shifts hit their limit before reaching the target. */
  limited: { hue: boolean; saturation: boolean; lightness: boolean };
  /** The target lies outside sRGB (an sRGB output maps it into sRGB). */
  targetOutsideSrgb: boolean;
  /** The target lies beyond the working gamut (the ProPhoto cube): no colour of the photo is it. */
  targetOutsideWorkingGamut: boolean;
  /** False when the range's sliders cannot reach the pick (its shifts are
   * then kept as they are): a range too grey for its own selection, or a
   * layer whose mask does not cover the pick (`uncovered`). */
  movable: boolean;
  uncovered: boolean;
}

/**
 * The readout's status of range `id` against `targetLab` with its shifts as
 * they are, without a search (the readout follows every slider move): the
 * distance, which sliders sit at their end while the target is missed,
 * whether the sliders reach the pick at all, and the target's gamut.
 */
export function targetStatus(
  ranges: ColorRange[],
  id: string,
  targetLab: Vec3,
  input?: RangeInput | null
): Omit<TargetSolve, 'shifts'> | null {
  const index = ranges.findIndex((r) => r.id === id);
  if (index < 0) return null;
  const at = input?.pixels.length ? input : centreInput(ranges[index]);
  const range = ranges[index];
  const { L, chroma, hue } = toLch(meanOklab(at.pixels));
  const ownWeight = weight(pack(range), L, chroma, hue);
  const strength = clamp(meanOf(at.influence), 0, 1);
  const movable = ownWeight * strength > 1e-6;
  const resultLab = renderedLab(ranges, at);
  const deltaE = deltaE00(targetLab, resultLab);
  const missed = deltaE > TARGET_REACHED;
  const end = (v: number | undefined) => missed && (!movable || Math.abs(num(v, 0)) >= SHIFT_LIMIT);
  const targetLinear = labToLinear(targetLab);
  return {
    resultLab,
    deltaE,
    limited: { hue: end(range.hueShift), saturation: end(range.saturation), lightness: end(range.lightnessShift) },
    targetOutsideSrgb: !insideSrgb(targetLinear),
    targetOutsideWorkingGamut: !insideWorkingGamut(targetLinear),
    movable,
    uncovered: !movable && ownWeight > 1e-6
  };
}

/**
 * The hue, saturation and lightness shifts of range `id` that move the
 * colour at its pick onto `targetLab` (CIELAB D50) with the other ranges as
 * they are. `input` is the renderer's probe at the pick (`RangeInput`);
 * without it the range's sampled colour stands for the pick at full
 * strength. Shifts beyond the sliders' limits are clamped and reported.
 */
export function solveColorTarget(
  ranges: ColorRange[],
  id: string,
  targetLab: Vec3,
  input?: RangeInput | null
): TargetSolve | null {
  const index = ranges.findIndex((r) => r.id === id);
  if (index < 0) return null;
  const at = input?.pixels.length ? input : centreInput(ranges[index]);
  const packed = ranges.map(pack);
  const own = packed[index];
  // The closed form at the pick's mean colour and mean influence.
  const { L: L0, chroma: c0, hue: h0 } = toLch(meanOklab(at.pixels));
  const strength = clamp(meanOf(at.influence), 0, 1);
  const others = accumulate(packed, L0, c0, h0, index);
  const ownWeight = weight(own, L0, c0, h0);
  // The range's own uniformity pulls toward its centre whatever its shifts.
  const fixed = {
    dh: others.dh + ownWeight * own.uh * wrap180(own.hue - h0),
    dc: others.dc + ownWeight * own.us * (own.chroma - c0),
    dl: others.dl + ownWeight * own.ul * (own.lightness - L0)
  };
  const w = ownWeight * strength;
  const targetLinear = labToLinear(targetLab);
  const targetOutsideSrgb = !insideSrgb(targetLinear);
  const target = toLch(linearToOklab(gamutUnit(targetLinear)));

  const steps = (value: number) => clamp(Math.round(value), -SHIFT_LIMIT, SHIFT_LIMIT);
  const limitedBy = (value: number) => Math.abs(value) > SHIFT_LIMIT + 0.5;

  // A layer's mix scales what the ranges move (exactly so for small moves):
  // hue + m·(fixed + w·dh), chroma · (1 + w·sat), L + m·(fixed + w·dl·2L).
  let hueSteps = 0;
  let satSteps = 0;
  let lightSteps = 0;
  const limited = { hue: false, saturation: false, lightness: false };
  if (w > 1e-6) {
    const base = Math.max(c0 + strength * fixed.dc, 0) * (1 + strength * (others.scale - 1));
    // A grey target has no hue: keep the hue, take the chroma away.
    if (target.chroma > 0.004) {
      const hue = wrap180(target.hue - h0 - strength * fixed.dh) / (w * HUE_PER_STEP);
      hueSteps = steps(hue);
      limited.hue = limitedBy(hue);
    }
    if (base > 1e-6) {
      const sat = ((target.chroma / base - 1) / w) * 100;
      satSteps = steps(sat);
      limited.saturation = limitedBy(sat);
    } else {
      limited.saturation = target.chroma > 0.004;
    }
    if (L0 > 1e-6) {
      const light = (target.L - L0 - strength * fixed.dl) / (w * 2 * L0 * LIGHTNESS_PER_STEP);
      lightSteps = steps(light);
      limited.lightness = limitedBy(light);
    } else {
      limited.lightness = target.L > 1e-3;
    }
  } else {
    limited.hue = limited.saturation = limited.lightness = true;
  }
  const withShifts = (s: Vec3) =>
    packed.map((p, i) =>
      i === index ? pack({ ...ranges[index], hueShift: s[0], saturation: s[1], lightnessShift: s[2] }) : p
    );
  const fitOf = (s: Vec3, over: RangeInput = at) => {
    const resultLab = renderedPacked(withShifts(s), over);
    return { resultLab, deltaE: deltaE00(targetLab, resultLab) };
  };
  const targetOutsideWorkingGamut = !insideWorkingGamut(targetLinear);
  const current: Vec3 = [
    num(ranges[index].hueShift, 0),
    num(ranges[index].saturation, 0),
    num(ranges[index].lightnessShift, 0)
  ];
  if (w <= 1e-6) {
    // Nothing the range's sliders do reaches the pick (a grey range, or a
    // layer whose mask no longer covers the point): its shifts stay.
    const fit = fitOf(current);
    return {
      shifts: { hueShift: current[0], saturation: current[1], lightnessShift: current[2] },
      resultLab: fit.resultLab,
      deltaE: fit.deltaE,
      limited: { hue: true, saturation: true, lightness: true },
      targetOutsideSrgb,
      targetOutsideWorkingGamut,
      movable: false,
      uncovered: ownWeight > 1e-6
    };
  }
  // Up to nine of the pixels with their influence: the coarse grid's stand-in
  // for the full model when it ranks starts and takes the first large steps
  // (a patch across an edge keeps both colours).
  const every = Math.max(1, Math.ceil(at.pixels.length / 9));
  const coarse: RangeInput = {
    pixels: at.pixels.filter((_, i) => i % every === 0),
    influence: at.influence.filter((_, i) => i % every === 0)
  };
  // The closed form matches hue, chroma and lightness each on its own at
  // the mean colour; the pixels around the pick, a layer's mix and the
  // working gamut's edge bend that, so a local search over whole slider
  // steps finishes it on the full model. When that does not reach the
  // target, the sliders together may (a patch across two colours, a target
  // at the gamut's edge, one slider beyond its reach): a coarse grid over all
  // three, then the local search from its best starts. A target inside the
  // working gamut takes that search's result only when it reaches the
  // target; otherwise the target goes as far as it can in its own direction
  // and the other sliders keep matching their own part (no colour drained
  // just to shave the distance).
  let best: Vec3 = [hueSteps, satSteps, lightSteps];
  let bestFit = fitOf(best);
  const all = [0, 1, 2];
  const descend = (start: Vec3, sizes: number[], over: RangeInput = at, free = all) => {
    let point = start;
    let fit = fitOf(point, over);
    for (const step of sizes) {
      for (let improved = true, rounds = 0; improved && rounds < 25; rounds += 1) {
        improved = false;
        for (const k of free) {
          for (const sign of [-1, 1]) {
            const next = [...point] as Vec3;
            next[k] = clamp(next[k] + sign * step, -SHIFT_LIMIT, SHIFT_LIMIT);
            if (next[k] === point[k]) continue;
            const nextFit = fitOf(next, over);
            if (nextFit.deltaE < fit.deltaE - 1e-9) {
              point = next;
              fit = nextFit;
              improved = true;
            }
          }
        }
      }
    }
    return { point, fit: over === at ? fit : fitOf(point) };
  };
  if (bestFit.deltaE > 0.05) {
    // Sliders the closed form ran out stay at their limit here.
    const open = all.filter((k) => ![limited.hue, limited.saturation, limited.lightness][k]);
    const found = descend(best, [4, 2, 1], at, open);
    if (found.fit.deltaE < bestFit.deltaE - 1e-9) ({ point: best, fit: bestFit } = found);
  }
  const closed = { best, bestFit };
  if (bestFit.deltaE > TARGET_REACHED) {
    // Steps of 20 over the three sliders: 1331 evaluations of the stand-in.
    const grid = [-100, -80, -60, -40, -20, 0, 20, 40, 60, 80, 100];
    const starts: { point: Vec3; deltaE: number }[] = [];
    for (const a of grid)
      for (const b of grid)
        for (const c of grid) starts.push({ point: [a, b, c], deltaE: fitOf([a, b, c], coarse).deltaE });
    starts.sort((x, y) => x.deltaE - y.deltaE);
    // From each start two ways down (the coarse sample's large steps, then
    // every pixel; every pixel throughout): they settle in different
    // valleys of a patch across an edge, and the better one counts.
    for (const start of starts.slice(0, 3)) {
      const rough = descend(start.point, [16, 8, 4], coarse);
      for (const found of [descend(rough.point, [2, 1]), descend(start.point, [16, 8, 4, 2, 1])])
        if (found.fit.deltaE < bestFit.deltaE - 1e-9) ({ point: best, fit: bestFit } = found);
    }
    const outside = targetOutsideWorkingGamut || !insideWorkingGamut(labToLinear(closed.bestFit.resultLab));
    if (!outside && bestFit.deltaE > TARGET_REACHED) ({ best, bestFit } = closed);
  }
  // A slider counts as run out only where the target stays out of reach.
  const missed = bestFit.deltaE > TARGET_REACHED;
  // The closed form's own limits, or, when the joint search gave the
  // result, the sliders it left at their end.
  const searched = best !== closed.best;
  const atLimit = (k: number) => Math.abs(best[k]) >= SHIFT_LIMIT;
  return {
    shifts: { hueShift: best[0], saturation: best[1], lightnessShift: best[2] },
    resultLab: bestFit.resultLab,
    deltaE: bestFit.deltaE,
    limited: {
      hue: missed && (searched ? atLimit(0) : limited.hue),
      saturation: missed && (searched ? atLimit(1) : limited.saturation),
      lightness: missed && (searched ? atLimit(2) : limited.lightness)
    },
    targetOutsideSrgb,
    targetOutsideWorkingGamut,
    movable: true,
    uncovered: false
  };
}

// --- Closed loop on the renderer's result ------------------------------

/** CIELAB D50 of the pickers' mean of sRGB display values (0–255 per pixel). */
export function meanDisplayLab(pixels: Vec3[]): Vec3 | null {
  if (!pixels.length) return null;
  const sum: Vec3 = [0, 0, 0];
  for (const p of pixels) for (let k = 0; k < 3; k += 1) sum[k] += p[k] / pixels.length;
  return linearToLab(srgb8ToLinear(sum));
}

/** The measured result of a range's target (`useEditorStore.colorTargetMeasure`):
 * the range (`photo|layer|range id`), the recipe measured (JSON) and the
 * CIELAB D50 the renderer left at the pick, or null when it could not be measured. */
export interface ColorTargetMeasure {
  key: string;
  recipe: string;
  /** Null when the renderer could not measure this recipe there (AI Denoise:
   * its region source is not the export's). */
  lab: Vec3 | null;
}

/** What the renderer leaves at the pick, in its sRGB render with every tool. */
export interface MeasuredTarget {
  lab: Vec3;
  /** ΔE00 to the target. */
  deltaE: number;
}

export interface MeasuredSolve {
  /** The ranges with the shifts that measured closest to the target. */
  ranges: ColorRange[];
  /** Their measurement, or null when the renderer could not measure (the
   * ranges are then the model's solve). */
  measured: MeasuredTarget | null;
  /** How many corrections after the model's solve were measured. */
  corrections: number;
}

/** Corrections after the model's solve (each one renderer probe). */
export const MAX_CORRECTIONS = 3;
/** No further correction starts once this much time has passed since the
 * first measurement came back (the corrections' share of a typed target's
 * latency; the first measurement may wait for the full-resolution source). */
export const CORRECTION_BUDGET_MS = 120;
/** Below this measured distance no correction is tried. */
const GOOD_ENOUGH = 0.2;

/**
 * The closed loop of the target solve: the model's solve
 * (`solveColorTarget`) is rendered and measured at the pick (`measure`: the
 * renderer's sRGB result there, every tool and the output's mapping
 * included), the aim is moved by the measured error and solved again, up
 * to `MAX_CORRECTIONS` times while the measured ΔE00 keeps improving and
 * the time budget lasts. Texture at the pick (the mean of shifted pixels
 * is not the shifted mean), later tools and the output's gamut mapping are
 * what the model leaves out; the measurement holds them all. Returns the
 * best measured ranges.
 */
export async function solveColorTargetMeasured(
  ranges: ColorRange[],
  id: string,
  targetLab: Vec3,
  input: RangeInput | null | undefined,
  measure: (ranges: ColorRange[]) => Promise<Vec3 | null>,
  { maxCorrections = MAX_CORRECTIONS, budgetMs = CORRECTION_BUDGET_MS, now = () => performance.now() } = {}
): Promise<MeasuredSolve | null> {
  const first = solveColorTarget(ranges, id, targetLab, input);
  if (!first) return null;
  const withShifts = (shifts: TargetSolve['shifts']) => ranges.map((r) => (r.id === id ? { ...r, ...shifts } : r));
  let best: MeasuredSolve = { ranges: withShifts(first.shifts), measured: null, corrections: 0 };
  let started: number | null = null;
  const tried = new Set<string>();
  let aim = targetLab;
  let solve = first;
  for (let step = 0; step <= maxCorrections; step += 1) {
    const { hueShift, saturation, lightnessShift } = solve.shifts;
    const key = `${hueShift}|${saturation}|${lightnessShift}`;
    // The same sliders again measure the same: the loop has settled.
    if (tried.has(key)) break;
    tried.add(key);
    const next = withShifts(solve.shifts);
    const lab = await measure(next);
    if (!lab || !lab.every(Number.isFinite)) break;
    const deltaE = deltaE00(targetLab, lab);
    if (best.measured && deltaE >= best.measured.deltaE - 0.01) break;
    best = { ranges: next, measured: { lab, deltaE }, corrections: step };
    started ??= now();
    if (deltaE <= GOOD_ENOUGH || now() - started > budgetMs) break;
    // Aim past the target by what the render missed it by.
    aim = aim.map((v, k) => v + (targetLab[k] - lab[k])) as Vec3;
    const again = solveColorTarget(ranges, id, aim, input);
    if (!again) break;
    solve = again;
  }
  return best;
}
