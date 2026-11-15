/**
 * Browser frames (Linux) at 100 %: a frame of the photograph's own size is
 * drawn one frame pixel per device pixel only when its pixels land on whole
 * device pixels. Anywhere between, WebKit filters it, and at a half-pixel
 * offset every pixel became the mean of four: the 100 % view matched the
 * export shifted by (½, ½) px bilinearly, 0.19× its sharpness (notes:
 * parity-gaps/G24-compare-before-after.md, LNX1).
 */

/** Whether the photograph is shown at one of its pixels per device pixel:
 * `zoom` the viewer's transform scale, `fitScale` CSS px per photograph px
 * at fit. */
export function isOneToOne(zoom: number, fitScale: number, dpr: number): boolean {
  return fitScale > 0 && Math.abs(zoom * fitScale * dpr - 1) < 1e-3;
}

/**
 * The pan (`position`, CSS px) nearest to it at which the photograph's
 * left (top) edge, `offset` CSS px into the scaled content, lies on a whole
 * device pixel, within [`min`, `max`]; `position` when none does.
 */
export function snapToDevicePixel(
  position: number,
  zoom: number,
  offset: number,
  dpr: number,
  min: number,
  max: number
): number {
  const edge = (position + zoom * offset) * dpr;
  const within = (p: number) => p >= min - 1e-6 && p <= max + 1e-6;
  for (const device of [Math.round(edge), Math.floor(edge), Math.ceil(edge)]) {
    const snapped = device / dpr - zoom * offset;
    if (within(snapped)) return Math.min(Math.max(snapped, min), max);
  }
  return position;
}

/** Whether a frame `frameWidth` px wide is drawn one pixel per device pixel:
 * the photograph shown 1:1 (`zoom`, the fit `size`) and the frame at its
 * full size. Such a frame is drawn unfiltered (`image-rendering: pixelated`):
 * the fit's fractional size can leave its pixels a little off whole device
 * pixels, which WebKit would filter. */
export function drawsOneToOne(
  frameWidth: number,
  size: { width: number; scale: number },
  zoom: number,
  dpr: number
): boolean {
  if (!(frameWidth > 0) || !isOneToOne(zoom, size.scale, dpr)) return false;
  return Math.abs(frameWidth - size.width / size.scale) <= 1;
}
