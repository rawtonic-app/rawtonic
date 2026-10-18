import type { Adjustments, Coord, MaskContainer } from './adjustments';

/** Committed adjustments with one mask layer replaced by its in-progress state.
 * Returns the committed adjustments unchanged when no layer has that id. */
export function withLiveMaskContainer(adjustments: Adjustments, container: MaskContainer): Adjustments {
  if (!adjustments.masks?.some((mask) => mask.id === container.id)) return adjustments;
  return {
    ...adjustments,
    masks: adjustments.masks.map((mask) => (mask.id === container.id ? container : mask)),
  };
}

/** An uncommitted brush stroke: one id per press, the layer and component it
 * is drawn on (its line is the component's last), and all its points so far
 * in image space. */
export interface LiveStroke {
  id: number;
  maskId: string;
  subMaskId: string;
  points: Coord[];
}

type LiveMaskRenderer = (adjustments: Adjustments, stroke?: LiveStroke) => void;
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
export function requestLiveMaskRender(adjustments: Adjustments, stroke?: LiveStroke) {
  liveMaskRenderer?.(adjustments, stroke);
}

/** The stroke part of a live frame (`src-tauri/src/live_stroke.rs`). With
 * the recipe, `from` is the number of points its line holds; without it,
 * `points` are those the stroke gained since `from`. */
export interface LiveStrokeArgs {
  id: number;
  maskId: string;
  subMaskId: string;
  from: number;
  points: Coord[];
}

/** The native side's reply to new points it cannot append (a frame was lost
 * or overtaken); the next frame sends the recipe again. */
export const LIVE_STROKE_RESYNC = 'Live stroke out of sync';

let strokeSent: { id: number; points: number } | null = null;

/** What the next live frame of `stroke` sends: the recipe once, then only the
 * points added since the previous frame, so a frame's payload does not grow
 * with the stroke or the layers. Frames go out in this order. */
export function liveStrokeFrame(stroke: LiveStroke): { withRecipe: boolean; args: LiveStrokeArgs } {
  const sent = strokeSent?.id === stroke.id && strokeSent.points <= stroke.points.length ? strokeSent.points : null;
  strokeSent = { id: stroke.id, points: stroke.points.length };
  const { id, maskId, subMaskId } = stroke;
  return sent === null
    ? { withRecipe: true, args: { id, maskId, subMaskId, from: stroke.points.length, points: [] } }
    : { withRecipe: false, args: { id, maskId, subMaskId, from: sent, points: stroke.points.slice(sent) } };
}

/** The native side no longer holds the stroke's recipe (it refused points,
 * or another frame without the stroke replaced it). */
export function forgetLiveStroke() {
  strokeSent = null;
}
