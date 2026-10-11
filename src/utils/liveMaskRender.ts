import type { Adjustments, MaskContainer } from './adjustments';

/** Committed adjustments with one mask layer replaced by its in-progress state.
 * Returns the committed adjustments unchanged when no layer has that id. */
export function withLiveMaskContainer(adjustments: Adjustments, container: MaskContainer): Adjustments {
  if (!adjustments.masks?.some((mask) => mask.id === container.id)) return adjustments;
  return {
    ...adjustments,
    masks: adjustments.masks.map((mask) => (mask.id === container.id ? container : mask)),
  };
}

type LiveMaskRenderer = (adjustments: Adjustments) => void;
let liveMaskRenderer: LiveMaskRenderer | null = null;

/** The preview pipeline registers how to render an uncommitted brush stroke.
 * Returns an unregister function that leaves a newer registration in place. */
export function registerLiveMaskRenderer(renderer: LiveMaskRenderer): () => void {
  liveMaskRenderer = renderer;
  return () => {
    if (liveMaskRenderer === renderer) liveMaskRenderer = null;
  };
}

/** Render adjustments containing an in-progress stroke without saving them. */
export function requestLiveMaskRender(adjustments: Adjustments) {
  liveMaskRenderer?.(adjustments);
}
