/** Positions the viewer may pan the photograph to, per axis. */
export interface PanBounds {
  minX: number;
  maxX: number;
  minY: number;
  maxY: number;
}

/** Axes on which the photograph fits the viewer and so cannot move (in the
 * fit view both: dragging there must not shift it, as in Lightroom and
 * Capture One). */
export function lockedAxes(bounds: PanBounds) {
  return { x: bounds.maxX - bounds.minX < 0.5, y: bounds.maxY - bounds.minY < 0.5 };
}

/** One pan step from (`x`, `y`) by the pointer's (`dx`, `dy`): nothing on a
 * locked axis, resisted beyond the edges of a free one. */
export function panStep(bounds: PanBounds, x: number, y: number, dx: number, dy: number) {
  const locked = lockedAxes(bounds);
  const resist = (pos: number, d: number, min: number, max: number) =>
    (pos < min && d < 0) || (pos > max && d > 0) ? d * 0.35 : d;
  return {
    dx: locked.x ? 0 : resist(x, dx, bounds.minX, bounds.maxX),
    dy: locked.y ? 0 : resist(y, dy, bounds.minY, bounds.maxY),
  };
}
