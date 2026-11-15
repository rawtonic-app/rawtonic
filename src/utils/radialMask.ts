// Capture One-style editing of a radial gradient mask. Three ellipses describe
// a mask: the inner line (full effect), the central line (50%, carrying the
// shape handles) and the outer line (no effect). A gradient without
// `gradientVersion` (version 1) has full coverage inside radius × (1 − feather)
// and falls linearly to zero at the radius. Version 2 (new gradients) falls
// off as a bell, `2^-(d / half)^(1 / softness)` of the elliptical distance d
// (`bell_falloff` in rawtonic-engine's mask_generation): its lines are drawn
// where 98%, 50% and 2% of the effect are left.

export interface RadialParams {
  centerX: number;
  centerY: number;
  radiusX: number;
  radiusY: number;
  rotation?: number;
  feather?: number;
  [key: string]: unknown;
}

export type RadialLine = 'inner' | 'outer';
export type RadialHandle = 'right' | 'left' | 'bottom' | 'top';

/** A point in the mask's own frame: x along radiusX, y along radiusY. */
export interface LocalPoint {
  x: number;
  y: number;
}

const MIN_RADIUS = 1;
const MAX_FEATHER = 1;

const clamp = (value: number, min: number, max: number) => Math.min(max, Math.max(min, value));

export function featherOf(params: RadialParams): number {
  const feather = params.feather;
  return typeof feather === 'number' && Number.isFinite(feather) ? clamp(feather, 0, MAX_FEATHER) : 0.5;
}

/** Whether the gradient falls off as a bell (`gradientVersion` 2+). */
function isBell(params: RadialParams): boolean {
  const version = params.gradientVersion;
  return typeof version === 'number' && version >= 2;
}

/** Feather → (50% distance in radii, reciprocal sharpness), linear in
 * between: the engine's `BELL_KNOTS`. */
const BELL_KNOTS: ReadonlyArray<readonly [number, number, number]> = [
  [0, 1, 0],
  [0.1, 0.991, 0.019],
  [0.25, 0.955, 0.067],
  [0.5, 0.885, 0.161],
  [0.75, 0.817, 0.247],
  [1, 0.786, 0.281]
];

function bellShape(feather: number): { half: number; softness: number } {
  let i = 0;
  while (i < BELL_KNOTS.length - 2 && BELL_KNOTS[i + 1][0] <= feather) i += 1;
  const [f0, h0, s0] = BELL_KNOTS[i];
  const [f1, h1, s1] = BELL_KNOTS[i + 1];
  const t = clamp((feather - f0) / (f1 - f0), 0, 1);
  return { half: h0 + (h1 - h0) * t, softness: s0 + (s1 - s0) * t };
}

/** The share of the effect left at elliptical distance `d` of a version 2
 * gradient (the engine's `bell_falloff`). */
export function bellCoverage(d: number, feather: number): number {
  if (feather <= 0) return d <= 1 ? 1 : 0;
  const { half, softness } = bellShape(feather);
  return Math.pow(2, -Math.pow(Math.max(d, 0) / half, 1 / softness));
}

/** Coverage left on the inner and outer lines of a version 2 gradient. */
const BELL_INNER = 0.98;
const BELL_OUTER = 0.02;

/** Inner, central and outer line in radii at `feather`. */
function lineFactors(params: RadialParams, feather: number) {
  if (!isBell(params)) return { inner: 1 - feather, central: 1 - feather / 2, outer: 1 };
  if (feather <= 0) return { inner: 1, central: 1, outer: 1 };
  const { half, softness } = bellShape(feather);
  // 2^-(d / half)^(1 / softness) = level  ⇔  d = half · log2(1 / level)^softness
  const at = (level: number) => half * Math.pow(Math.log2(1 / level), softness);
  return { inner: at(BELL_INNER), central: half, outer: at(BELL_OUTER) };
}

/** Radii of the inner (full), central (50%) and outer (none) lines. */
export function radialLines(params: RadialParams) {
  const f = lineFactors(params, featherOf(params));
  const scaled = (factor: number) => ({ x: params.radiusX * factor, y: params.radiusY * factor });
  return { inner: scaled(f.inner), central: scaled(f.central), outer: scaled(f.outer) };
}

/** The feather in [0, 1] at which `ratio(feather)` (monotonic) reaches
 * `target`, clamped to the ends of its range. */
function solveFeather(ratio: (feather: number) => number, target: number): number {
  const rising = ratio(1) > ratio(0);
  let lo = 0;
  let hi = MAX_FEATHER;
  for (let i = 0; i < 40; i += 1) {
    const mid = (lo + hi) / 2;
    if (ratio(mid) < target === rising) lo = mid;
    else hi = mid;
  }
  return (lo + hi) / 2;
}

/** `dragRadialLine` for a version 2 gradient: the same rules, with the
 * lines at the bell's 98 / 50 / 2% points. */
function dragBellLine(params: RadialParams, line: RadialLine, point: LocalPoint, symmetric: boolean): RadialParams {
  const lines = radialLines(params);
  const at = (feather: number) => lineFactors(params, feather);
  // The line that stays, its radii and its factor as a function of feather.
  const [kept, keptFactor, movedFactor] = symmetric
    ? [lines.central, (f: number) => at(f).central, line === 'inner' ? (f: number) => at(f).inner : (f: number) => at(f).outer]
    : line === 'inner'
      ? [lines.outer, (f: number) => at(f).outer, (f: number) => at(f).inner]
      : [lines.inner, (f: number) => at(f).inner, (f: number) => at(f).outer];
  const d = ellipticalDistance(point, kept);
  const feather = solveFeather((f) => movedFactor(f) / keptFactor(f), d);
  const reached = movedFactor(feather) / keptFactor(feather);
  // Past the farthest the feather reaches (outer line far out), the moved
  // line goes to the pointer and the whole mask scales with it.
  const scale = line === 'outer' && d > reached ? d / reached : 1;
  const factor = keptFactor(feather);
  return withRadii(params, (kept.x * scale) / factor, (kept.y * scale) / factor, feather);
}

function angle(params: RadialParams) {
  return ((params.rotation ?? 0) * Math.PI) / 180;
}

/** Image coordinates to the mask's rotated frame, as the engine evaluates it. */
export function toLocal(params: RadialParams, x: number, y: number): LocalPoint {
  const t = angle(params);
  const dx = x - params.centerX;
  const dy = y - params.centerY;
  return { x: dx * Math.cos(t) + dy * Math.sin(t), y: -dx * Math.sin(t) + dy * Math.cos(t) };
}

/** The mask's rotated frame back to image coordinates. */
export function toImage(params: RadialParams, point: LocalPoint): LocalPoint {
  const t = angle(params);
  return {
    x: params.centerX + point.x * Math.cos(t) - point.y * Math.sin(t),
    y: params.centerY + point.x * Math.sin(t) + point.y * Math.cos(t)
  };
}

/** Image positions of the four shape handles on the central line. */
export function radialHandlePositions(params: RadialParams): Record<RadialHandle, LocalPoint> {
  const { central } = radialLines(params);
  return {
    right: toImage(params, { x: central.x, y: 0 }),
    left: toImage(params, { x: -central.x, y: 0 }),
    bottom: toImage(params, { x: 0, y: central.y }),
    top: toImage(params, { x: 0, y: -central.y })
  };
}

/** How far out a point lies, relative to an ellipse with these radii (1 = on it). */
function ellipticalDistance(point: LocalPoint, radii: LocalPoint) {
  return Math.hypot(point.x / Math.max(radii.x, MIN_RADIUS), point.y / Math.max(radii.y, MIN_RADIUS));
}

function withRadii(params: RadialParams, outerX: number, outerY: number, feather: number): RadialParams {
  return {
    ...params,
    radiusX: Math.max(MIN_RADIUS, outerX),
    radiusY: Math.max(MIN_RADIUS, outerY),
    feather: clamp(feather, 0, MAX_FEATHER)
  };
}

/**
 * Drag the inner or outer line through `point` (mask frame). Without
 * `symmetric` the other line stays where it is: the inner line changes only
 * the feather, the outer line the overall size. With `symmetric` (Shift in
 * Capture One) the central line stays and both lines move apart or together.
 */
export function dragRadialLine(
  params: RadialParams,
  line: RadialLine,
  point: LocalPoint,
  symmetric = false
): RadialParams {
  if (isBell(params)) return dragBellLine(params, line, point, symmetric);
  const feather = featherOf(params);
  const { inner, central, outer } = radialLines(params);
  if (symmetric) {
    const d = ellipticalDistance(point, central);
    // inner = central·(1 − f)/(1 − f/2), outer = central/(1 − f/2)
    const next =
      line === 'inner'
        ? (1 - clamp(d, 0, 1)) / (1 - clamp(d, 0, 1) / 2)
        : clamp(2 * (1 - 1 / Math.max(d, 1)), 0, MAX_FEATHER);
    const centralFactor = 1 - next / 2;
    return withRadii(params, central.x / centralFactor, central.y / centralFactor, next);
  }
  if (line === 'inner') {
    return withRadii(params, outer.x, outer.y, 1 - clamp(ellipticalDistance(point, outer), 0, 1));
  }
  if (feather >= 0.98) {
    // (Almost) no 100% area to keep: the outer line scales the whole mask.
    const scale = ellipticalDistance(point, outer);
    return withRadii(params, outer.x * scale, outer.y * scale, feather);
  }
  const scale = Math.max(1, ellipticalDistance(point, inner));
  return withRadii(params, inner.x * scale, inner.y * scale, 1 - 1 / scale);
}

/**
 * Drag one of the four central-line handles to `point` (mask frame). The
 * handle's axis is resized around the centre; `lockOpposite` (Alt) keeps the
 * opposite handle in place and moves the centre instead; `uniform` (Shift)
 * scales the whole mask. The feather fraction is kept.
 */
export function dragRadialHandle(
  params: RadialParams,
  handle: RadialHandle,
  point: LocalPoint,
  options: { uniform?: boolean; lockOpposite?: boolean } = {}
): RadialParams {
  const feather = featherOf(params);
  const { central } = radialLines(params);
  const horizontal = handle === 'right' || handle === 'left';
  const sign = handle === 'right' || handle === 'bottom' ? 1 : -1;
  const reach = sign * (horizontal ? point.x : point.y);
  const current = horizontal ? central.x : central.y;
  // With the opposite handle fixed, the central radius spans half the distance.
  const nextCentral = Math.max(MIN_RADIUS, options.lockOpposite ? (reach + current) / 2 : reach);
  // The central line is proportional to the radius, so both scale alike.
  const scale = nextCentral / Math.max(current, MIN_RADIUS);
  const scaleX = horizontal || options.uniform ? scale : 1;
  const scaleY = !horizontal || options.uniform ? scale : 1;
  const next = withRadii(params, params.radiusX * scaleX, params.radiusY * scaleY, feather);
  if (!options.lockOpposite) return next;
  const shift = (nextCentral - current) * sign;
  const centre = toImage(params, horizontal ? { x: shift, y: 0 } : { x: 0, y: shift });
  return { ...next, centerX: centre.x, centerY: centre.y };
}
