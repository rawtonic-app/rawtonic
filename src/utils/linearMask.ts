// Capture One-style editing of a linear gradient mask. The engine's central
// (50%) line runs through start → end; coverage is 100% at `range` on the
// start side and 0% at `rangeEnd` (default `range`) on the other side. Capture
// One calls these the start, central and end lines.

export interface LinearParams {
  startX: number;
  startY: number;
  endX: number;
  endY: number;
  range?: number;
  rangeEnd?: number;
  [key: string]: unknown;
}

export type LinearLine = 'start' | 'end';
export type LinearLineDrag = 'opposite' | 'independent' | 'symmetric';

interface Point {
  x: number;
  y: number;
}

const MIN_RANGE = 0.5;
const SNAP = Math.PI / 4;

export function rangesOf(params: LinearParams) {
  const start = Math.max(MIN_RANGE, Number.isFinite(params.range) ? (params.range as number) : 50);
  const end = Number.isFinite(params.rangeEnd) ? Math.max(MIN_RANGE, params.rangeEnd as number) : start;
  return { start, end };
}

/** Unit vectors along the central line and towards the end (0%) side. */
export function linearFrame(params: LinearParams) {
  const vx = params.endX - params.startX;
  const vy = params.endY - params.startY;
  const length = Math.hypot(vx, vy) || 1;
  return { along: { x: vx / length, y: vy / length }, normal: { x: -vy / length, y: vx / length } };
}

/** Signed distance from the central line; negative on the start (100%) side. */
export function signedDistance(params: LinearParams, point: Point) {
  const { normal } = linearFrame(params);
  return (point.x - params.startX) * normal.x + (point.y - params.startY) * normal.y;
}

/** Offsets of the start and end lines from the central line along the normal. */
export function linearLineOffsets(params: LinearParams) {
  const { start, end } = rangesOf(params);
  return { start: -start, end };
}

function shifted(params: LinearParams, offset: number): LinearParams {
  const { normal } = linearFrame(params);
  return {
    ...params,
    startX: params.startX + normal.x * offset,
    startY: params.startY + normal.y * offset,
    endX: params.endX + normal.x * offset,
    endY: params.endY + normal.y * offset,
  };
}

function withRanges(params: LinearParams, start: number, end: number): LinearParams {
  const next: LinearParams = { ...params, range: Math.max(MIN_RANGE, start) };
  const endRange = Math.max(MIN_RANGE, end);
  if (Math.abs(endRange - next.range!) < 1e-9) delete next.rangeEnd;
  else next.rangeEnd = endRange;
  return next;
}

/**
 * Drag the start (100%) or end (0%) line through `point` (image coordinates).
 * 'opposite' keeps the other line and the ratio of the two transition zones,
 * moving the central line; 'independent' (Alt) keeps the central line and
 * changes only the dragged zone; 'symmetric' (Shift) keeps the central line
 * and gives both zones the dragged width.
 */
export function dragLinearLine(params: LinearParams, line: LinearLine, point: Point, mode: LinearLineDrag = 'opposite') {
  const { start, end } = rangesOf(params);
  const distance = signedDistance(params, point);
  // How far the dragged line lies from the central line, on its own side.
  const reach = Math.max(MIN_RANGE, line === 'start' ? -distance : distance);
  if (mode === 'independent') return line === 'start' ? withRanges(params, reach, end) : withRanges(params, start, reach);
  if (mode === 'symmetric') return withRanges(params, reach, reach);
  // Keep the other line where it is; the central line keeps its relative place.
  const fixed = line === 'start' ? end : -start;
  const moving = line === 'start' ? Math.min(distance, fixed - 2 * MIN_RANGE) : Math.max(distance, fixed + 2 * MIN_RANGE);
  const total = Math.abs(fixed - moving);
  const startShare = start / (start + end);
  const nextStart = total * startShare;
  const nextEnd = total - nextStart;
  // Offset of the new central line from the old one.
  const central = line === 'start' ? fixed - nextEnd : fixed + nextStart;
  return withRanges(shifted(params, central), nextStart, nextEnd);
}

/**
 * Rotate the mask around `pivot` by the change of the pointer's angle since
 * the drag began; `snap` (Shift) locks the central line to 45° steps.
 */
export function rotateLinear(params: LinearParams, pivot: Point, from: Point, to: Point, snap = false): LinearParams {
  let delta = Math.atan2(to.y - pivot.y, to.x - pivot.x) - Math.atan2(from.y - pivot.y, from.x - pivot.x);
  if (snap) {
    const current = Math.atan2(params.endY - params.startY, params.endX - params.startX);
    delta = Math.round((current + delta) / SNAP) * SNAP - current;
  }
  const cos = Math.cos(delta);
  const sin = Math.sin(delta);
  const turn = (x: number, y: number) => ({
    x: pivot.x + (x - pivot.x) * cos - (y - pivot.y) * sin,
    y: pivot.y + (x - pivot.x) * sin + (y - pivot.y) * cos,
  });
  const start = turn(params.startX, params.startY);
  const end = turn(params.endX, params.endY);
  return { ...params, startX: start.x, startY: start.y, endX: end.x, endY: end.y };
}

/**
 * Capture One draws a linear gradient from the 100% point (`from`) to the 0%
 * point (`to`); the central line lies halfway and runs across the drag. `snap`
 * (Shift) locks the drag direction to 45° steps. `handle` sets how far the
 * stored start/end points lie from the middle (they only orient the line).
 */
export function linearFromDrag(params: LinearParams, from: Point, to: Point, handle: number, snap = false): LinearParams {
  let dx = to.x - from.x;
  let dy = to.y - from.y;
  let length = Math.hypot(dx, dy);
  if (snap && length > 0) {
    const angle = Math.round(Math.atan2(dy, dx) / SNAP) * SNAP;
    dx = Math.cos(angle) * length;
    dy = Math.sin(angle) * length;
  }
  length = Math.max(length, 2 * MIN_RANGE);
  const unit = Math.hypot(dx, dy) > 0 ? { x: dx / Math.hypot(dx, dy), y: dy / Math.hypot(dx, dy) } : { x: 0, y: 1 };
  const middle = { x: from.x + (unit.x * length) / 2, y: from.y + (unit.y * length) / 2 };
  // The engine's normal points towards the end side: start → end runs along
  // (unit.y, −unit.x) so that its left normal equals `unit`.
  const along = { x: unit.y, y: -unit.x };
  const next: LinearParams = {
    ...params,
    startX: middle.x - along.x * handle,
    startY: middle.y - along.y * handle,
    endX: middle.x + along.x * handle,
    endY: middle.y + along.y * handle,
    range: length / 2,
  };
  delete next.rangeEnd;
  return next;
}
