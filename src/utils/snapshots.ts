import type { TFunction } from 'i18next';
import { debouncedSetHistory, useEditorStore } from '../store/useEditorStore';
import { useSnapshotStore, type Snapshot } from '../store/useSnapshotStore';
import { nameHistoryStep } from '../components/studio/historyLabels';
import { updateAdjustments } from '../hooks/useEditorActions';
import { normalizeLoadedAdjustments, type Adjustments, type AiPatch, type MaskContainer } from './adjustments';
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

/** Restores `snapshot` on the open photograph: its adjustments become the
 * edit as one history step named after it, so Undo returns to the edit
 * before (Lightroom's snapshots). False when no photograph is ready. */
export function restoreSnapshot(snapshot: Snapshot): boolean {
  const image = useEditorStore.getState().selectedImage;
  if (!image?.isReady || useSnapshotStore.getState().path !== image.path) return false;
  const restored = withOwnBitmapIds(normalizeLoadedAdjustments(snapshot.adjustments, image.isRaw));
  // An edit still settling is its own step, before the restore.
  debouncedSetHistory.flush();
  updateAdjustments(() => restored);
  debouncedSetHistory.flush();
  const { history, historyIndex } = useEditorStore.getState();
  const step = history[historyIndex];
  if (step) nameHistoryStep(step, (t: TFunction) => t('studio.develop.snapshots.restored', { name: snapshot.name }));
  return true;
}

/** A new snapshot's name: when it was taken, as Lightroom names them. */
export function defaultSnapshotName(now: Date, language: string): string {
  return now.toLocaleString(language, { dateStyle: 'short', timeStyle: 'medium' });
}

/** Takes a snapshot of `adjustments` (the current edit or a history step's)
 * on the open photograph; resolves to it, or null when none was taken. */
export async function takeSnapshot(name: string, adjustments: Adjustments): Promise<Snapshot | null> {
  const image = useEditorStore.getState().selectedImage;
  if (!image?.isReady) return null;
  return useSnapshotStore.getState().create(image.path, name, adjustments);
}
