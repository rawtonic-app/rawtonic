import type { MaskContainer } from "./adjustments";

/** The engine renders at most this many layers (`MAX_MASKS` in
 * crates/engine/src/image_processing.rs); later ones are kept but ignored by
 * previews and exports. */
export const MAX_RENDERED_LAYERS = 32;

type LayerLike = Pick<MaskContainer, "id" | "visible" | "subMasks">;

/** A layer the engine renders: visible with at least one component (the
 * engine's `produces_bitmap`). */
export const isRenderedLayer = (layer: LayerLike) => layer.visible && layer.subMasks.length > 0;

/** Whether one more rendered layer still fits. */
export const canRenderAnotherLayer = (masks: readonly LayerLike[]) =>
  masks.filter(isRenderedLayer).length < MAX_RENDERED_LAYERS;

/** The ids of layers the engine skips: rendered ones after the first
 * `MAX_RENDERED_LAYERS` in application order (only older recipes have them). */
export function unrenderedLayerIds(masks: readonly LayerLike[]): Set<string> {
  const ids = new Set<string>();
  let rendered = 0;
  for (const layer of masks) {
    if (!isRenderedLayer(layer)) continue;
    rendered += 1;
    if (rendered > MAX_RENDERED_LAYERS) ids.add(layer.id);
  }
  return ids;
}

/** `next`, unless it renders more layers than `prev` beyond the limit: then
 * `prev` (the last guard of an action that adds one, on the state it
 * actually changes). */
export function keepWithinLayerLimit<T extends { masks: readonly LayerLike[] }>(prev: T, next: T): T {
  const rendered = (masks: readonly LayerLike[]) => masks.filter(isRenderedLayer).length;
  const after = rendered(next.masks);
  return after > MAX_RENDERED_LAYERS && after > rendered(prev.masks) ? prev : next;
}
