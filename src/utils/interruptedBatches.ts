// SPDX-License-Identifier: AGPL-3.0-or-later
/**
 * Batch edits a quit, kill or crash cut short (G66): the backend records
 * every multi-photo batch (paste, sync, preset, style, rating, colour
 * label, keyword) before its first photograph is written
 * (src-tauri/src/batch_journal.rs). One left behind is offered when its
 * library opens, under Needs attention: "Interrupted: … applied to N of M
 * photos", with Apply to the rest, Undo the applied part and Dismiss. A
 * batch is never left half applied without the photographer knowing.
 *
 * Both repairs write through the photographs' write queues with the
 * ordinary batch commands (so they are recorded as batches themselves);
 * the job is forgotten only once its repair was written. A repair that
 * fails says why and offers the batch again.
 */
import i18n from 'i18next';
import { invoke } from '@tauri-apps/api/core';
import { Invokes } from '../components/ui/AppProperties';
import { useActivityStore } from '../store/useActivityStore';
import { useLibraryStore } from '../store/useLibraryStore';
import { notify } from '../activity/notify';
import { invokeInOrder } from './catalogWrites';
import { catalogChanged } from './catalogEvents';
import { submitPhotoWrite } from './photoWrites';

/** What the batch wrote (`BatchKind` in batch_journal.rs). */
export type InterruptedBatchKind =
  | { kind: 'paste' }
  | { kind: 'style' }
  | { kind: 'preset' }
  | { kind: 'rating'; rating: number }
  | { kind: 'color'; color: string | null }
  | { kind: 'keywordAdd'; tag: string }
  | { kind: 'keywordRemove'; tag: string };

/** `InterruptedBatch` in batch_journal.rs. */
export interface InterruptedBatch {
  id: string;
  batch: InterruptedBatchKind;
  command: string;
  args: Record<string, unknown>;
  startedMs: number;
  total: number;
  applied: number;
  /** The photographs it changed, with their value before. */
  undo: { path: string; before: unknown }[];
  /** The photographs it did not reach. */
  rest: string[];
}

const activityId = (batch: InterruptedBatch) => `batch:interrupted:${batch.id}`;

/** The batch's name in a sentence. */
function batchName({ batch }: InterruptedBatch): string {
  switch (batch.kind) {
    case 'rating':
      return i18n.t('activity.interrupted.rating', { rating: batch.rating });
    case 'color':
      return i18n.t('activity.interrupted.color');
    case 'keywordAdd':
      return i18n.t('activity.interrupted.keywordAdd', { tag: batch.tag });
    case 'keywordRemove':
      return i18n.t('activity.interrupted.keywordRemove', { tag: batch.tag });
    default:
      return i18n.t(`activity.interrupted.${batch.kind}`);
  }
}

/** Throws when a batch command reports photographs it did not write. */
function written(result: unknown) {
  const failure = (result as { failure?: { failed?: unknown } } | null)?.failure;
  if (failure) throw new Error(JSON.stringify(failure));
}

/** Groups paths by the value they get back. */
function byValue<V>(entries: { path: string; before: unknown }[], value: (before: unknown) => V) {
  const groups = new Map<string, { value: V; paths: string[] }>();
  for (const entry of entries) {
    const v = value(entry.before);
    const key = JSON.stringify(v);
    const group = groups.get(key) ?? { value: v, paths: [] };
    group.paths.push(entry.path);
    groups.set(key, group);
  }
  return [...groups.values()];
}

/** Writes the batch onto the photographs it did not reach. */
export async function applyToRest(batch: InterruptedBatch) {
  if (batch.rest.length === 0) return;
  const args = { ...batch.args, paths: batch.rest };
  if (batch.command === Invokes.ApplyAdjustmentsToPaths) {
    written(await submitPhotoWrite(batch.rest, () => invoke(batch.command, args), { kind: batch.batch.kind }));
    catalogChanged();
  } else {
    await invokeInOrder(batch.command, args, batch.batch.kind);
  }
}

/** Writes back what the batch changed, field by field (the rest of each
 * photograph's metadata stays as it is now). */
export async function undoApplied(batch: InterruptedBatch) {
  const { undo } = batch;
  if (undo.length === 0) return;
  const paths = undo.map((entry) => entry.path);
  const directory = (batch.args as { directory?: unknown }).directory ?? undefined;
  switch (batch.batch.kind) {
    case 'paste':
    case 'style':
    case 'preset': {
      const changes = undo.map((entry) => ({ path: entry.path, adjustments: entry.before }));
      written(await submitPhotoWrite(paths, () => invoke(Invokes.ReplaceRecipes, { changes }), { kind: 'restore' }));
      catalogChanged();
      return;
    }
    case 'rating':
      for (const group of byValue(undo, (before) => Number(before) || 0))
        await invokeInOrder(Invokes.SetRatingForPaths, { paths: group.paths, rating: group.value, directory });
      return;
    case 'color':
      for (const group of byValue(undo, (before) => (typeof before === 'string' ? before : null)))
        await invokeInOrder(Invokes.SetColorLabelForPaths, { paths: group.paths, color: group.value, directory });
      return;
    case 'keywordAdd':
      await invokeInOrder(Invokes.RemoveTagForPaths, { paths, tag: batch.batch.tag }, 'keyword');
      return;
    case 'keywordRemove':
      await invokeInOrder(Invokes.AddTagForPaths, { paths, tag: batch.batch.tag }, 'keyword');
      return;
  }
}

/** Puts `batch` under Needs attention with its three choices; `refresh`
 * shows the photographs anew after a repair. */
export function offerInterruptedBatch(batch: InterruptedBatch, refresh: () => Promise<void> | void = () => {}) {
  const name = batchName(batch);
  const forget = () =>
    invoke(Invokes.DismissInterruptedBatch, { id: batch.id }).catch((error) =>
      console.warn('Could not forget the interrupted batch:', error),
    );
  const settle = (repair: (batch: InterruptedBatch) => Promise<void>) => async () => {
    try {
      await repair(batch);
      await invoke(Invokes.DismissInterruptedBatch, { id: batch.id });
    } catch (error) {
      notify.error(i18n.t('activity.interrupted.failed', { batch: name, error: String(error) }));
      offerInterruptedBatch(batch, refresh);
    } finally {
      await Promise.resolve(refresh()).catch(() => {});
    }
  };
  useActivityStore.getState().report('warning', {
    id: activityId(batch),
    source: 'write',
    title: i18n.t('activity.interrupted.title', { batch: name, applied: batch.applied, count: batch.total }),
    detail: i18n.t('activity.interrupted.detail'),
    paths: [...batch.undo.map((entry) => entry.path), ...batch.rest],
    actions: [
      ...(batch.rest.length
        ? [{ label: i18n.t('activity.interrupted.applyRest'), settles: true, run: settle(applyToRest) }]
        : []),
      ...(batch.undo.length
        ? [{ label: i18n.t('activity.interrupted.undoApplied'), settles: true, run: settle(undoApplied) }]
        : []),
      { label: i18n.t('activity.dismissShort'), settles: true, run: forget },
    ],
    // Its close button declines it as Dismiss does.
    onDismiss: forget,
  });
}

const offered = new Set<string>();

/** Offers the batches earlier runs left in `directory` (the library that
 * just opened; null: none is open), each once per app run. */
export async function offerInterruptedBatches(directory: string | null, refresh?: () => Promise<void> | void) {
  const batches = await invoke<InterruptedBatch[]>(Invokes.InterruptedBatches, { directory }).catch((error) => {
    console.warn('Could not list interrupted batches:', error);
    return null;
  });
  if (!Array.isArray(batches) || useLibraryStore.getState().sessionDirectory !== directory) return;
  for (const batch of batches) {
    if (offered.has(batch.id)) continue;
    offered.add(batch.id);
    // Every photograph reached after all (a kill after the last write):
    // nothing to choose.
    if (batch.rest.length === 0) {
      void invoke(Invokes.DismissInterruptedBatch, { id: batch.id }).catch(() => {});
      continue;
    }
    offerInterruptedBatch(batch, refresh);
  }
}

/** Test helper. */
export function resetInterruptedBatches() {
  offered.clear();
}
