/**
 * A compositing hint (`will-change: transform`) for an element only while it
 * moves, dropped once it rests.
 *
 * WebKitGTK rasterises a composited layer at its untransformed size and lets
 * the compositor scale that bitmap: under the viewer's `scale(4)` (100 % of a
 * 24 MP photo in a 1500 px wide viewer) a full-resolution frame was shown
 * from a ~1500 px raster, 4× magnified — the after pane far softer than the
 * export and than the before pane, which had no hint (notes: parity-gaps/
 * G24-compare-before-after.md). Without the hint at rest the frame is painted
 * through the transform at screen resolution; during a pan or zoom the layer
 * still moves on the compositor without repainting the photo each move.
 */
const SETTLE_MS = 200;

const timers = new WeakMap<HTMLElement, number>();

/** Composites `element` now and until it has not moved for `settleMs`. */
export function compositeWhileMoving(element: HTMLElement, settleMs = SETTLE_MS) {
  if (element.style.willChange !== 'transform') element.style.willChange = 'transform';
  const pending = timers.get(element);
  if (pending !== undefined) window.clearTimeout(pending);
  timers.set(
    element,
    window.setTimeout(() => {
      timers.delete(element);
      element.style.willChange = '';
    }, settleMs)
  );
}
