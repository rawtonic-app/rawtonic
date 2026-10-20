import { Mask, SubMask } from '../components/panel/right/Masks';
import { linearFromDrag, type LinearParams } from './linearMask';

type Point = { x: number; y: number };

/** Screen pixels a press must travel to draw a shape; less is a click,
 * which keeps the shape the tool created. */
export const SHAPE_DRAG_THRESHOLD = 15;

/**
 * A new gradient drawn by one drag on the photo (Lightroom, Capture One), in
 * image pixels: a radial from its centre (the press) out to the pointer, a
 * linear from its 100% point (the press) to its 0% point (`linearFromDrag`).
 * Null while the drag is shorter than `SHAPE_DRAG_THRESHOLD` screen pixels,
 * unless it has been longer already (`drawing`): once drawing, the shape
 * follows the pointer back towards the press. `handle` places a linear's
 * orientation points; `snap` (Shift) locks its direction to 45° steps.
 */
export function shapeFromDrag(
  type: Mask,
  params: SubMask['parameters'],
  from: Point,
  to: Point,
  { scale, handle, snap = false, drawing = false }: { scale: number; handle: number; snap?: boolean; drawing?: boolean },
): SubMask['parameters'] | null {
  if (!drawing && Math.hypot(to.x - from.x, to.y - from.y) < SHAPE_DRAG_THRESHOLD / scale) return null;
  if (type === Mask.Radial) {
    return {
      ...params,
      centerX: from.x,
      centerY: from.y,
      radiusX: Math.max(1, Math.abs(to.x - from.x)),
      radiusY: Math.max(1, Math.abs(to.y - from.y)),
      rotation: 0,
    };
  }
  if (type === Mask.Linear) return linearFromDrag(params as LinearParams, from, to, handle, snap);
  return null;
}

/** A gradient being drawn: the component the drag started on and its shape
 * so far (no draft until the drag passes the threshold). */
export interface ShapeDraft {
  id: string;
  params: SubMask['parameters'];
}

/** What the release commits to the active component `activeId`: only its own
 * draft, never one a drag drew for a component active before (a tool key
 * pressed mid-drag). Null: nothing changes. */
export function draftToCommit(draft: ShapeDraft | null, activeId: string): SubMask['parameters'] | null {
  return draft && draft.id === activeId ? draft.params : null;
}

/** A gradient drag in progress: the component it draws, whether it has
 * passed the click threshold, and whether its shape was rendered into the
 * developed preview (native live render) rather than committed. */
export interface ShapeDragState {
  id: string;
  drawing?: boolean;
  renderedLive?: boolean;
}

/**
 * A gradient drag cancelled before its release (another component, tool or
 * mode became active): the preview returns to the recipe, which never held
 * the draft. A live-rendered draft is replaced by a render of the recipe as
 * it is, whatever mode the editor is in now; the drawn layer's overlay shows
 * its committed shape.
 */
export function restoreAfterShapeDrag<A extends { masks: { subMasks: { id: string }[] }[] }>(
  drag: ShapeDragState,
  recipe: A,
  { renderLive, showLayer }: { renderLive(recipe: A): void; showLayer(layer: A['masks'][number]): void },
) {
  const layer = recipe.masks.find((m) => m.subMasks.some((sub) => sub.id === drag.id));
  if (drag.renderedLive) renderLive(recipe);
  if (layer) showLayer(layer);
}
