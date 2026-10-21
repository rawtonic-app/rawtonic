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
