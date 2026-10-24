import type { Adjustments, AiPatch, MaskContainer } from './adjustments';
import type { SubMask } from '../components/panel/right/Masks';

const bitmapOf = (sub: SubMask): unknown => {
  const parameters = sub.parameters as Record<string, unknown> | undefined;
  return parameters?.maskDataBase64 ?? parameters?.mask_data_base64 ?? null;
};

/** The bitmaps of `adjustments` by component or patch id. */
function bitmapsById(adjustments: Adjustments | undefined): Map<string, unknown> {
  const bitmaps = new Map<string, unknown>();
  const subs = (list: SubMask[] | undefined) => list?.forEach((sub) => bitmaps.set(sub.id, bitmapOf(sub)));
  adjustments?.masks?.forEach((mask: MaskContainer) => subs(mask.subMasks));
  adjustments?.aiPatches?.forEach((patch: AiPatch) => {
    bitmaps.set(patch.id, patch.patchData ? JSON.stringify(patch.patchData) : null);
    subs(patch.subMasks);
  });
  return bitmaps;
}

/** `adjustments` with a new id for each AI bitmap (mask component, AI
 * patch): the native side keeps bitmaps by id and the preview leaves out
 * one it has sent (or is sending) under an id, so a bitmap computed again
 * since `adjustments` were made (a snapshot, a pasted recipe) would be shown
 * instead. Under ids of their own the restored bitmaps always arrive. One
 * that `shown` (the recipe on screen) holds as it is under its id keeps it:
 * that is the bitmap the native side has. */
export function withOwnBitmapIds(adjustments: Adjustments, shown?: Adjustments): Adjustments {
  const kept = bitmapsById(shown);
  const own = <T extends { id: string }>(item: T, bitmap: unknown): T =>
    bitmap && !(kept.has(item.id) && kept.get(item.id) === bitmap) ? { ...item, id: crypto.randomUUID() } : item;
  const subMasks = (list: SubMask[] | undefined) => list?.map((sub) => own(sub, bitmapOf(sub)));
  return {
    ...adjustments,
    masks: adjustments.masks?.map((mask: MaskContainer) => ({ ...mask, subMasks: subMasks(mask.subMasks) ?? [] })),
    aiPatches: adjustments.aiPatches?.map((patch: AiPatch) => ({
      ...own(patch, patch.patchData ? JSON.stringify(patch.patchData) : null),
      subMasks: subMasks(patch.subMasks) ?? patch.subMasks,
    })),
  };
}

/** Sent in place of a mask bitmap the native side already holds under the
 * component's id (`CACHED_BITMAP` in crates/engine/src/adjustment_utils.rs).
 * A null bitmap means the component has none and renders nothing. */
export const CACHED_BITMAP = 'cached';
const MASK_DATA_KEYS = ['maskDataBase64', 'mask_data_base64'] as const;

/** In `subMasks` (a copy about to be sent to the native side), refers to
 * each mask bitmap already `sent` under its component's id instead of
 * sending it again; returns the ids carrying their bitmap for the first time,
 * to be added to `sent` once the request went through. A component without a
 * bitmap (a People selection with its last part unticked) makes the native
 * side drop the one under its id, so it is forgotten in `sent` too: a bitmap
 * computed under that id later goes out whole. */
export function referToSentMaskBitmaps(subMasks: SubMask[] | undefined, sent: Set<string>): string[] {
  const newlySent: string[] = [];
  if (!Array.isArray(subMasks)) return newlySent;
  for (const sub of subMasks) {
    const parameters = sub.parameters as Record<string, unknown> | undefined;
    if (!sub.id || !parameters) continue;
    const keys = MASK_DATA_KEYS.filter((key) => key in parameters);
    if (keys.length === 0) continue;
    if (keys.every((key) => parameters[key] === null || parameters[key] === undefined)) {
      sent.delete(sub.id);
    } else if (sent.has(sub.id)) {
      for (const key of keys) if (parameters[key] != null) parameters[key] = CACHED_BITMAP;
    } else {
      newlySent.push(sub.id);
    }
  }
  return newlySent;
}
