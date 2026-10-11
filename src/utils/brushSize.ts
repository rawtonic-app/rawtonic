/** Brush sizes are diameters in image pixels, as in Capture One (up to 6000),
 * so a brush covers the same part of the photograph at every zoom. */
export const BRUSH_SIZE_MIN = 1;
export const BRUSH_SIZE_MAX = 6000;
export const DEFAULT_BRUSH_SIZE = 250;

/** One `[` / `]` (or Control+arrow) step: about 15% of the size, at least one
 * pixel, so small brushes stay controllable and large ones change quickly. */
export function stepBrushSize(size: number, direction: 1 | -1) {
  const step = Math.max(1, Math.round(size * 0.15));
  return Math.min(BRUSH_SIZE_MAX, Math.max(BRUSH_SIZE_MIN, Math.round(size) + direction * step));
}
