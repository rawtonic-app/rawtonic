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

const clamp = (v: number, lo: number, hi: number) => Math.min(hi, Math.max(lo, v));
const smoothstep = (e0: number, e1: number, x: number) => {
  const t = clamp((x - e0) / (e1 - e0), 0, 1);
  return t * t * (3 - 2 * t);
};
// WGSL's `%` keeps the dividend's sign, as JavaScript's does.
const wrap180 = (d: number) => (((d % 360) + 540) % 360) - 180;

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
  const { L, chroma, hue } = toLch(lab);
  return finish(L, chroma, hue, accumulate(ranges.map(pack), L, chroma, hue));
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

/** Oklab of a range's sampled colour. */
export const rangeCentre = (range: ColorRange): Vec3 => {
  const h = (range.hue * Math.PI) / 180;
  return [range.lightness, range.chroma * Math.cos(h), range.chroma * Math.sin(h)];
};

export interface TargetSolve {
  /** The range's new shifts (whole slider steps). */
  shifts: Pick<ColorRange, 'hueShift' | 'saturation' | 'lightnessShift'>;
  /** The range centre's colour after the solve (Oklab, before clipping to sRGB). */
  result: Vec3;
  /** CIELAB D50 of the result clipped to sRGB, and its ΔE00 to the target. */
  resultLab: Vec3;
  deltaE: number;
  /** Which shifts hit their limit before reaching the target. */
  limited: { hue: boolean; saturation: boolean; lightness: boolean };
  /** The target lies outside sRGB (it is shown and reached clipped). */
  targetOutsideSrgb: boolean;
  /** False for a range too grey for its own selection (nothing moves). */
  movable: boolean;
}

/**
 * The hue, saturation and lightness shifts of range `id` that move its
 * sampled colour onto `targetLab` (CIELAB D50) with the other ranges as
 * they are. Shifts beyond the sliders' limits are clamped and reported.
 */
export function solveColorTarget(ranges: ColorRange[], id: string, targetLab: Vec3): TargetSolve | null {
  // Only the ranges the renderer reads count (it takes the first eight).
  const index = ranges.findIndex((r) => r.id === id);
  if (index < 0) return null;
  const packed = ranges.map(pack);
  const own = packed[index];
  const L0 = own.lightness;
  const c0 = own.chroma;
  const h0 = own.hue;
  const others = accumulate(packed, L0, c0, h0, index);
  const w = weight(own, L0, c0, h0);
  const targetLinear = labToLinear(targetLab);
  const targetOutsideSrgb = !insideSrgb(targetLinear);
  const target = toLch(linearToOklab(targetLinear));

  const steps = (value: number) => clamp(Math.round(value), -SHIFT_LIMIT, SHIFT_LIMIT);
  const limitedBy = (value: number) => Math.abs(value) > SHIFT_LIMIT + 0.5;

  // The range's own uniformity pulls nothing at its centre, so its shifts
  // act alone: hue + w·dh, chroma · (1 + w·sat), L + w·dl·2L.
  let hueSteps = 0;
  let satSteps = 0;
  let lightSteps = 0;
  const limited = { hue: false, saturation: false, lightness: false };
  if (w > 1e-6) {
    const base = Math.max(c0 + others.dc, 0) * others.scale;
    // A grey target has no hue: keep the hue, take the chroma away.
    if (target.chroma > 0.004) {
      const hue = wrap180(target.hue - h0 - others.dh) / (w * HUE_PER_STEP);
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
      const light = (target.L - L0 - others.dl) / (w * 2 * L0 * LIGHTNESS_PER_STEP);
      lightSteps = steps(light);
      limited.lightness = limitedBy(light);
    } else {
      limited.lightness = target.L > 1e-3;
    }
  } else {
    limited.hue = limited.saturation = limited.lightness = true;
  }
  const centre = rangeCentre(ranges[index]);
  const evaluate = (steps: Vec3) => {
    const solved = ranges.map((r, i) =>
      i === index ? { ...r, hueShift: steps[0], saturation: steps[1], lightnessShift: steps[2] } : r
    );
    const result = applyColorRanges(solved, centre);
    const resultLab = linearToLab(oklabToLinear(result).map((v) => clamp(v, 0, 1)) as Vec3);
    return { result, resultLab, deltaE: deltaE00(targetLab, resultLab) };
  };
  // The closed form matches hue, chroma and lightness each on its own. When
  // its result or the target lies outside sRGB, the colour shown is the
  // clipped one, and the nearest of those needs the sliders together: a
  // pattern search over whole slider steps from there (a few hundred
  // evaluations of the model at most). Sliders at their limit stay there,
  // so an out-of-reach target goes as far as it can in its own direction,
  // and inside sRGB the others keep matching their own part.
  let best: Vec3 = [hueSteps, satSteps, lightSteps];
  let bestFit = evaluate(best);
  const free = [!limited.hue, !limited.saturation, !limited.lightness].flatMap((open, k) => (open ? [k] : []));
  if (w > 1e-6 && (targetOutsideSrgb || !insideSrgb(oklabToLinear(bestFit.result)))) {
    for (const step of [32, 16, 8, 4, 2, 1]) {
      for (let improved = true, rounds = 0; improved && rounds < 25; rounds += 1) {
        improved = false;
        for (const k of free) {
          for (const sign of [-1, 1]) {
            const next = [...best] as Vec3;
            next[k] = clamp(next[k] + sign * step, -SHIFT_LIMIT, SHIFT_LIMIT);
            if (next[k] === best[k]) continue;
            const fit = evaluate(next);
            if (fit.deltaE < bestFit.deltaE - 1e-9) {
              best = next;
              bestFit = fit;
              improved = true;
            }
          }
        }
      }
    }
  }
  return {
    shifts: { hueShift: best[0], saturation: best[1], lightnessShift: best[2] },
    result: bestFit.result,
    resultLab: bestFit.resultLab,
    deltaE: bestFit.deltaE,
    limited,
    targetOutsideSrgb,
    movable: w > 1e-6
  };
}
