import type { Adjustments, AiPatch, MaskContainer } from './adjustments';
import type { SubMask } from '../components/panel/right/Masks';

/** `adjustments` with a new id for each AI bitmap (mask component, AI
 * patch): the native side keeps bitmaps by id and the preview leaves out
 * one it has sent (or is sending) under an id, so a bitmap computed again
 * since the snapshot was taken would be shown instead of the restored one.
 * Under ids of their own the restored bitmaps always arrive. */
export function withOwnBitmapIds(adjustments: Adjustments): Adjustments {
  const own = <T extends { id: string }>(item: T, hasBitmap: boolean): T =>
    hasBitmap ? { ...item, id: crypto.randomUUID() } : item;
  const subMasks = (list: SubMask[] | undefined) =>
    list?.map((sub) => {
      const parameters = sub.parameters as Record<string, unknown> | undefined;
      return own(sub, Boolean(parameters?.maskDataBase64 ?? parameters?.mask_data_base64));
    });
  return {
    ...adjustments,
    masks: adjustments.masks?.map((mask: MaskContainer) => ({ ...mask, subMasks: subMasks(mask.subMasks) ?? [] })),
    aiPatches: adjustments.aiPatches?.map((patch: AiPatch) => ({
      ...own(patch, Boolean(patch.patchData)),
      subMasks: subMasks(patch.subMasks) ?? patch.subMasks,
    })),
  };
}
