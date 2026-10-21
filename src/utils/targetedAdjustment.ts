import {
  CURVE_VERSION,
  INITIAL_GRAY_MIXER,
  INITIAL_REGION_CURVE,
  MAX_CURVE_POINTS,
  type Adjustments,
  type Coord,
  type GrayMixer,
  type RegionCurve,
  editedCurvesVersion,
} from "./adjustments";
import { pointCurveValue } from "./pointCurve";
import { regionAt, regionCurveValue } from "./regionCurve";

/**
 * Lightroom's Targeted Adjustment Tool (Capture One's curve and colour
 * pickers): drag up or down in the photo and the sliders that act on the
 * colour or tone under the pointer move. Hue, Saturation and Luminance of
 * the colour mixer, the black & white mix, the luma tone curve and the
 * region curve (the region of the tone under the pointer, as Lightroom's
 * parametric curve).
 */
export type TatTarget =
  "hue" | "saturation" | "luminance" | "grayMixer" | "curve" | "regionCurve";

/** Targets that act on the displayed tone rather than the photo's colour. */
export const isToneTarget = (target: TatTarget) =>
  target === "curve" || target === "regionCurve";

export const HSL_CHANNELS = [
  "reds",
  "oranges",
  "yellows",
  "greens",
  "aquas",
  "blues",
  "purples",
  "magentas",
] as const;
const GRAY_CHANNELS = [
  "red",
  "orange",
  "yellow",
  "green",
  "aqua",
  "blue",
  "purple",
  "magenta",
] as const;

// Channel windows as the shader selects colours (apply_hsl_panel,
// apply_black_and_white): Oklab hue centres and raised-cosine reach.
const HSL_CENTRES = [15, 52, 105, 145, 195, 262, 305, 340];
const HSL_REACH = [32, 38, 50, 55, 55, 50, 40, 38];
const BW_CENTRES = [18.6, 55.2, 94.3, 134.9, 181.9, 236.0, 312.1, 348.0];
const BW_REACH = [33.9, 41.8, 38.7, 41.4, 42.1, 84.8, 60.6, 31.1];

/** Slider units per screen pixel of vertical drag (Lightroom-like pace). */
export const TAT_UNITS_PER_PIXEL = 0.5;
/** Colours closer to grey than this (Oklab chroma) have no channel. */
const MIN_CHROMA = 0.02;

const toLinear = (v: number) =>
  v <= 0.04045 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4;

/** Oklab lightness, chroma and hue (degrees) of a linear sRGB colour. */
export function oklch([r, g, b]: [number, number, number]) {
  const l = Math.cbrt(
    Math.max(0, 0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b),
  );
  const m = Math.cbrt(
    Math.max(0, 0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b),
  );
  const s = Math.cbrt(
    Math.max(0, 0.0883024619 * r + 0.2817188376 * g + 0.6299787 * b),
  );
  const L = 0.2104542553 * l + 0.793617785 * m - 0.0040720468 * s;
  const A = 1.9779984951 * l - 2.428592205 * m + 0.4505937099 * s;
  const B = 0.0259040371 * l + 0.7827717662 * m - 0.808675766 * s;
  return {
    lightness: L,
    chroma: Math.hypot(A, B),
    hue: ((Math.atan2(B, A) * 180) / Math.PI + 360) % 360,
  };
}

/** Each channel's share of a hue, scaled so the strongest channel is 1. */
function channelWeights(hue: number, centres: number[], reach: number[]) {
  const w = centres.map((centre, i) => {
    const distance = Math.abs(((((hue - centre) % 360) + 540) % 360) - 180);
    return 0.5 + 0.5 * Math.cos(Math.min(distance / reach[i], 1) * Math.PI);
  });
  const max = Math.max(...w);
  return w.map((x) => (max > 0 ? x / max : 0));
}

export interface TatStart {
  target: TatTarget;
  /** Channel weights (colour targets) of the sampled colour. */
  weights?: number[];
  /** Curve input level (0–255) under the pointer (curve target). */
  curveX?: number;
  /** The adjustments when the drag began. */
  base: Adjustments;
}

/**
 * Prepares a drag from the colour under the pointer: `linearRgb` is the
 * photo's colour before adjustments (colour targets), `curveInput` the
 * colour the region and point curves receive there, linear sRGB that may
 * reach beyond 0–1 (tone targets; editor_preview.rs `sample_curve_input`,
 * rendered without the curves and what follows them). Returns null for
 * grey under a colour target or a missing sample.
 */
export function startTargetedAdjustment(
  target: TatTarget,
  base: Adjustments,
  linearRgb: [number, number, number] | null,
  curveInput: [number, number, number] | null,
): TatStart | null {
  if (isToneTarget(target)) {
    if (!curveInput) return null;
    // The region curve and curves on display values see the colour
    // clipped to sRGB.
    const display = curveInput.map((v) => toEncoded(v) * 255) as [
      number,
      number,
      number,
    ];
    if (target === "regionCurve")
      return { target, base, curveX: Math.round(tone(display, false)) };
    // The point curve acts on the region curve's output (when the curve
    // tool is on), in the space the edited curves render in.
    const region =
      base.sectionVisibility?.curves !== false
        ? regionOf(base)
        : INITIAL_REGION_CURVE;
    const regioned = display.map(
      (v) => regionCurveValue(region, v / 255) * 255,
    ) as [number, number, number];
    if (editedCurvesVersion(base.curves) < CURVE_VERSION)
      return { target, base, curveX: Math.round(tone(regioned, false)) };
    // The wide curves also see the part beyond sRGB, carried past the
    // region curve at its luminance ratio (shader `carry_beyond_srgb`).
    const clipped = display.map((v) => toLinear(v / 255));
    const shown = regioned.map((v) => toLinear(v / 255));
    const ratio = Math.min(
      luminance(shown) / Math.max(luminance(clipped), 1e-5),
      4,
    );
    const carried = shown.map(
      (v, c) => v + (curveInput[c] - clipped[c]) * ratio,
    ) as [number, number, number];
    return {
      target,
      base,
      curveX: Math.round(tone(wideCurveLevels(carried), true)),
    };
  }
  if (!linearRgb) return null;
  const { chroma, hue } = oklch(linearRgb);
  if (chroma < MIN_CHROMA) return null;
  const weights =
    target === "grayMixer"
      ? channelWeights(hue, BW_CENTRES, BW_REACH)
      : channelWeights(hue, HSL_CENTRES, HSL_REACH);
  return { target, base, weights };
}

/** The colour mixer channel a drag acts on most (for the panel to show). */
export function dominantChannel(start: TatStart): string | null {
  if (
    !start.weights ||
    start.target === "grayMixer" ||
    isToneTarget(start.target)
  )
    return null;
  return HSL_CHANNELS[start.weights.indexOf(Math.max(...start.weights))];
}

const clamp = (v: number, lo: number, hi: number) =>
  Math.max(lo, Math.min(hi, v));
const round = (v: number) => Math.round(v * 10) / 10;

/** The adjustments after dragging `delta` slider units (up = positive). */
export function targetedPatch(
  start: TatStart,
  delta: number,
): Partial<Adjustments> {
  const { base, target } = start;
  if (target === "regionCurve") {
    const region = regionOf(base);
    const key = regionAt(region, (start.curveX ?? 128) / 255);
    return {
      regionCurve: {
        ...region,
        [key]: Math.round(clamp(region[key] + delta, -100, 100)),
      },
    };
  }
  if (target === "curve") {
    return {
      curves: {
        ...base.curves,
        luma: moveCurve(base.curves.luma, start.curveX ?? 128, delta),
        version: editedCurvesVersion(base.curves),
      },
      curveMode: "point",
    };
  }
  const weights = start.weights ?? [];
  if (target === "grayMixer") {
    const mixer: GrayMixer = { ...INITIAL_GRAY_MIXER, ...base.grayMixer };
    GRAY_CHANNELS.forEach((channel, i) => {
      if (weights[i] > 0.02)
        mixer[channel] = round(
          clamp(mixer[channel] + delta * weights[i], -100, 100),
        );
    });
    return { grayMixer: mixer, blackAndWhite: true };
  }
  const hsl = { ...base.hsl };
  HSL_CHANNELS.forEach((channel, i) => {
    if (weights[i] > 0.02) {
      hsl[channel] = {
        ...hsl[channel],
        [target]: round(
          clamp(hsl[channel][target] + delta * weights[i], -100, 100),
        ),
      };
    }
  });
  return { hsl };
}

const regionOf = (a: Adjustments): RegionCurve => ({
  ...INITIAL_REGION_CURVE,
  ...a.regionCurve,
});

/** The renderer's luma point curve at `x` (0–255). */
export const curveValue = pointCurveValue;

const toEncoded = (v: number) => {
  const c = Math.min(Math.max(v, 0), 1);
  return c <= 0.0031308 ? c * 12.92 : 1.055 * c ** (1 / 2.4) - 0.055;
};
// Linear sRGB to linear ProPhoto (Bradford D65 to D50), the shader's
// `srgb_to_prophoto`.
const TO_PROPHOTO = [
  [0.5293458, 0.3300728, 0.1405812],
  [0.0983744, 0.8734611, 0.0281647],
  [0.0168832, 0.1176725, 0.8654443],
];

const luminance = (linear: number[]) =>
  0.2126 * linear[0] + 0.7152 * linear[1] + 0.0722 * linear[2];

/** Linear sRGB (beyond 0–1 for colours outside sRGB) as the wide curves
 * receive it, in levels 0–255: ProPhoto primaries with the sRGB curve
 * (shader `wide_curve_input`). */
export function wideCurveLevels(
  linear: [number, number, number],
): [number, number, number] {
  return TO_PROPHOTO.map(
    (row) =>
      toEncoded(row[0] * linear[0] + row[1] * linear[1] + row[2] * linear[2]) *
      255,
  ) as [number, number, number];
}

/** The tone of curve levels: their luminance-weighted mean, with the
 * luminance of the space the curves act in. */
const tone = (levels: number[], wide: boolean) => {
  const weights = wide
    ? [0.2880402, 0.7118741, 0.0000857]
    : [0.2126, 0.7152, 0.0722];
  return levels.reduce((sum, level, c) => sum + weights[c] * level, 0);
};

/** Moves the curve at `x` by `delta`: the point there, or a new one. */
function moveCurve(
  points: Coord[] | undefined,
  x: number,
  delta: number,
): Coord[] {
  const p =
    points && points.length >= 2
      ? points.map((q) => ({ ...q }))
      : [
          { x: 0, y: 0 },
          { x: 255, y: 255 },
        ];
  // The nearest inner point within reach (imported curves can hold points
  // a level apart).
  let near = -1;
  p.forEach((q, i) => {
    if (i === 0 || i === p.length - 1 || Math.abs(q.x - x) > 6) return;
    if (near === -1 || Math.abs(q.x - x) < Math.abs(p[near].x - x)) near = i;
  });
  if (near !== -1) {
    p[near].y = clamp(Math.round(p[near].y + delta), 0, 255);
    return p;
  }
  if (x <= 0 || x >= 255 || p.length >= MAX_CURVE_POINTS) return p;
  const y = clamp(Math.round(curveValue(p, x) + delta), 0, 255);
  return [...p, { x, y }].sort((a, b) => a.x - b.x);
}

export const linearFromSrgb = (
  rgb: [number, number, number],
): [number, number, number] =>
  rgb.map((v) => toLinear(v / 255)) as [number, number, number];
