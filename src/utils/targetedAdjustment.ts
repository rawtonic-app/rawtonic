import {
  CURVE_VERSION,
  INITIAL_GRAY_MIXER,
  INITIAL_REGION_CURVE,
  MAX_CURVE_POINTS,
  type Adjustments,
  type Coord,
  type GrayMixer,
  type RegionCurve,
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
 * photo's colour before adjustments (colour targets), `displayRgb` the
 * displayed colour in 0–255 (curve). Returns null for grey under a colour
 * target or a missing sample.
 */
export function startTargetedAdjustment(
  target: TatTarget,
  base: Adjustments,
  linearRgb: [number, number, number] | null,
  displayRgb: [number, number, number] | null,
): TatStart | null {
  if (isToneTarget(target)) {
    // The displayed sample can be traced back through the photo's curves
    // only: layer curves and a LUT after them would mislead it.
    if (!displayRgb || hasToneAfterCurves(base)) return null;
    const region = regionOf(base);
    const curves = base.curves;
    const channelCurves = [curves?.red, curves?.green, curves?.blue];
    // Each channel went through the region, luma and its own curve: recover
    // its level before them, then weigh the channels into one tone.
    const before = displayRgb.map((shown, c) =>
      invertCurve(
        (x) =>
          pointCurveValue(
            channelCurves[c],
            pointCurveValue(
              curves?.luma,
              regionCurveValue(region, x / 255) * 255,
            ),
          ),
        shown,
      ),
    );
    const weights = [0.2126, 0.7152, 0.0722];
    const tone = (levels: number[]) =>
      levels.reduce((sum, level, c) => sum + weights[c] * level, 0);
    // The point curve acts on the region curve's output.
    return {
      target,
      base,
      curveX: Math.round(
        target === "regionCurve"
          ? tone(before)
          : tone(before.map((x) => regionCurveValue(region, x / 255) * 255)),
      ),
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
      },
      curveMode: "point",
      curveVersion: CURVE_VERSION,
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

const isIdentity = (points: Coord[] | undefined) =>
  !points ||
  points.length < 2 ||
  (points.every((p) => p.x === p.y) &&
    points[0].x === 0 &&
    points[points.length - 1].x === 255);

/** A visible layer with curves, or a LUT: tones changed after the photo's
 * curves, which the displayed sample cannot be traced back through. */
function hasToneAfterCurves(a: Adjustments) {
  const layerCurves = (a.masks ?? []).some((mask) => {
    const curves =
      mask.visible === false ? undefined : mask.adjustments?.curves;
    return (
      !!curves &&
      [curves.luma, curves.red, curves.green, curves.blue].some(
        (c) => !isIdentity(c as Coord[] | undefined),
      )
    );
  });
  return layerCurves || !!a.lutPath;
}

const regionOf = (a: Adjustments): RegionCurve => ({
  ...INITIAL_REGION_CURVE,
  ...a.regionCurve,
});

/** The renderer's luma point curve at `x` (0–255). */
export const curveValue = pointCurveValue;

/** The input level `curve` maps to `displayed` (nearest for flat parts). */
function invertCurve(curve: (x: number) => number, displayed: number): number {
  let best = 0;
  let error = Infinity;
  for (let x = 0; x <= 255; x += 1) {
    const e = Math.abs(curve(x) - displayed);
    if (e < error) {
      error = e;
      best = x;
    }
  }
  return best;
}

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
  const near = p.findIndex(
    (q, i) => i > 0 && i < p.length - 1 && Math.abs(q.x - x) <= 6,
  );
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
