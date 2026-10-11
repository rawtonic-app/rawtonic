// Capture One-style editing of a radial gradient mask. The engine draws full
// coverage inside radius × (1 − feather) and falls linearly to zero at the
// radius, so three ellipses describe a mask: the inner line (100%), the central
// line (50%, carrying the shape handles) and the outer line (0%).

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

/** Radii of the inner (100%), central (50%) and outer (0%) lines. */
export function radialLines(params: RadialParams) {
  const feather = featherOf(params);
  const scaled = (factor: number) => ({ x: params.radiusX * factor, y: params.radiusY * factor });
  return { inner: scaled(1 - feather), central: scaled(1 - feather / 2), outer: scaled(1) };
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
    y: params.centerY + point.x * Math.sin(t) + point.y * Math.cos(t),
  };
}

/** Image positions of the four shape handles on the central line. */
export function radialHandlePositions(params: RadialParams): Record<RadialHandle, LocalPoint> {
  const { central } = radialLines(params);
  return {
    right: toImage(params, { x: central.x, y: 0 }),
    left: toImage(params, { x: -central.x, y: 0 }),
    bottom: toImage(params, { x: 0, y: central.y }),
    top: toImage(params, { x: 0, y: -central.y }),
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
    feather: clamp(feather, 0, MAX_FEATHER),
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
  symmetric = false,
): RadialParams {
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
  options: { uniform?: boolean; lockOpposite?: boolean } = {},
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
