// SPDX-License-Identifier: AGPL-3.0-or-later
/**
 * Feeds the activity centre from the app's existing reporting paths that
 * keep their own state: the export queue, AI tasks, AI Denoise, model
 * downloads, library indexing, batch commands and the photo write queue.
 * Paths that only say something once go through `notify` instead.
 */
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import i18n from 'i18next';
import { Invokes } from '../components/ui/AppProperties';
import { useActivityStore } from '../store/useActivityStore';
import { useEditorStore } from '../store/useEditorStore';
import { useProcessStore, type ExportJob } from '../store/useProcessStore';
import {
  batchPhotoCount,
  onPhotoBatchChanged,
  onPhotoWritesSettled,
  type PhotoBatch,
} from '../utils/photoWrites';

const fileName = (path: string) => {
  const file = path.split('?vc=')[0];
  return file.slice(Math.max(file.lastIndexOf('/'), file.lastIndexOf('\\')) + 1);
};

/** At most `count` names, then "(+N)". */
function names(paths: string[], count = 5): string {
  const listed = paths.slice(0, count).map(fileName).join(', ');
  return paths.length > count ? `${listed} (+${paths.length - count})` : listed;
}

const act = (command: Invokes, args: Record<string, unknown>) =>
  invoke(command, args).catch((error) =>
    useActivityStore.getState().report('failure', { source: 'export', title: String(error) }),
  );

/** What each export job last reported, so a snapshot repeating it (or one
 * after the user dismissed its failure) reports nothing new. */
const exportReported = new Map<string, string>();

export function syncExportJobs(jobs: ExportJob[], held: boolean) {
  const activity = useActivityStore.getState();
  const ids = new Set(jobs.map((job) => job.id));
  for (const id of [...exportReported.keys()]) {
    if (ids.has(id)) continue;
    exportReported.delete(id);
    // A removed job's task goes; what it failed stays until dismissed.
    if (activity.entries.some((entry) => entry.id === `export:${id}` && entry.status === 'running'))
      activity.resolve(`export:${id}`);
  }
  for (const job of jobs) {
    const id = `export:${job.id}`;
    const waiting = job.state === 'queued' && !held;
    const key = job.state === 'running' || waiting ? `${job.state}:${job.done}:${job.failed}` : job.state;
    if (exportReported.get(job.id) === key) continue;
    exportReported.set(job.id, key);
    if (job.state === 'running' || waiting) {
      const fields = {
        source: 'export' as const,
        title: i18n.t(job.state === 'running' ? 'activity.export.running' : 'activity.export.waiting', { label: job.label }),
        progress: { done: job.done + job.failed, total: job.total },
        cancel: () => act(Invokes.ExportQueueCancel, { job: job.id }),
      };
      if (activity.entries.some((entry) => entry.id === id && entry.status === 'running')) activity.update(id, fields);
      else activity.start({ id, ...fields });
    } else if (job.state === 'failed') {
      const failures = job.failures ?? [];
      activity.report('failure', {
        id,
        source: 'export',
        title: i18n.t('activity.export.failed', { count: job.failed, label: job.label }),
        detail: failures.slice(0, 3).map((failure) => `${fileName(failure.path)}: ${failure.error}`).join('\n') || undefined,
        paths: failures.map((failure) => failure.path),
        actions: [{ label: i18n.t('activity.tryAgain'), settles: true, run: () => act(Invokes.ExportQueueResume, { job: job.id }) }],
      });
    } else if (job.state === 'interrupted') {
      // The app quit during it: it waits for Resume.
      activity.report('warning', {
        id,
        source: 'export',
        title: i18n.t('activity.export.interrupted', { label: job.label }),
        actions: [{ label: i18n.t('activity.resume'), settles: true, run: () => act(Invokes.ExportQueueResume, { job: job.id }) }],
      });
    } else if (job.state === 'done') {
      // The written files are reported once per run with Show in Finder
      // (`notifyExportOutputs`); the task just ends.
      activity.finish(id);
    } else {
      // Cancelled or paused by the user, or waiting while the queue is held.
      activity.resolve(id);
    }
  }
}

const AI_TASKS = ['subject', 'depth', 'foreground', 'sky', 'people', 'inpaint', 'object'] as const;
type AiTaskKind = (typeof AI_TASKS)[number] | 'other';

/** The AI tasks running (masks, Generative Fill), one entry each. */
export function syncAiTasks(tasks: Record<string, { type: string }>) {
  const activity = useActivityStore.getState();
  for (const entry of activity.entries)
    if (entry.id.startsWith('ai:task:') && !(entry.id.slice('ai:task:'.length) in tasks)) activity.finish(entry.id);
  for (const [id, task] of Object.entries(tasks)) {
    const entryId = `ai:task:${id}`;
    if (activity.entries.some((entry) => entry.id === entryId)) continue;
    const kind: AiTaskKind = (AI_TASKS as readonly string[]).includes(task.type) ? (task.type as AiTaskKind) : 'other';
    activity.start({ id: entryId, source: 'ai', title: i18n.t(`activity.ai.${kind}`) });
  }
}

/** The photographs a write could not save, and its last reason. */
const unsavedWrites = new Map<string, string>();
let lastWriteError: string | null = null;
const WRITES_ID = 'write:not-saved';

/** The reason a failed write gives (the command's own message): for the
 * next failed write, or, with `paths` the write queue already reported, for
 * those photographs at once (a caller that learns why only after the queue
 * settled), so one failure is one entry. */
export function noteWriteError(message: string, paths?: string[]) {
  if (paths?.length && paths.every((path) => unsavedWrites.has(path))) {
    for (const path of paths) unsavedWrites.set(path, message);
    showUnsavedWrites(true);
    return;
  }
  if (paths?.length) {
    // Not a write of the queue: its own entry, so it is never lost.
    useActivityStore.getState().report('failure', { source: 'write', title: message, paths });
    return;
  }
  lastWriteError = message;
}

/** A failed write stays under Needs attention until each of its
 * photographs is written after all (a later write of the same values or
 * newer ones) or the user dismisses it. */
export function syncPhotoWrite(paths: string[], failed: string[]) {
  const failedSet = new Set(failed);
  for (const path of paths) if (!failedSet.has(path)) unsavedWrites.delete(path);
  const reason = lastWriteError;
  lastWriteError = null;
  for (const path of failed) unsavedWrites.set(path, reason ?? unsavedWrites.get(path) ?? '');
  showUnsavedWrites(failed.length > 0);
}

/** The one entry of the photographs not saved (`fresh`: a new failure,
 * shown again after a dismissal). */
function showUnsavedWrites(fresh: boolean) {
  const activity = useActivityStore.getState();
  const shown = activity.entries.find((entry) => entry.id === WRITES_ID);
  if (!unsavedWrites.size) {
    if (shown) activity.resolve(WRITES_ID);
    return;
  }
  if (!shown && !fresh) return;
  const unsaved = [...unsavedWrites.keys()];
  const reasons = [...new Set([...unsavedWrites.values()].filter(Boolean))];
  activity.report('failure', {
    id: WRITES_ID,
    source: 'write',
    title: i18n.t('activity.writes.failed', { count: unsaved.length }),
    detail: [names(unsaved), ...reasons.slice(0, 3)].join('\n'),
    paths: unsaved,
  });
}

/** Test helper. */
export function resetActivityBridges() {
  exportReported.clear();
  unsavedWrites.clear();
  lastWriteError = null;
}

function syncBatch(batch: PhotoBatch, finished: boolean) {
  const activity = useActivityStore.getState();
  const id = `batch:${batch.id}`;
  if (finished) {
    activity.finish(id);
    return;
  }
  const { done, total } = batchPhotoCount(batch);
  const title = i18n.t(batch.kind === 'auto' || batch.kind === 'faceRetouch' ? `activity.batch.${batch.kind}` : 'activity.batch.other', {
    count: total,
  });
  if (activity.entries.some((entry) => entry.id === id)) activity.update(id, { title, progress: { done, total } });
  else activity.start({ id, source: 'batch', title, progress: { done, total } });
}

/** Tauri's `listen`, quiet without the native side (tests, `npm run dev`). */
function listenSafely<T>(event: string, handler: (payload: T) => void): Promise<() => void> {
  return Promise.resolve()
    .then(() => listen<T>(event, ({ payload }) => handler(payload)))
    .catch(() => () => {});
}

type DownloadProgress = { model: string; received: number; total: number; batchReceived?: number; batchTotal?: number };

/** Starts every bridge; returns their stop. */
export function startActivityBridges(): () => void {
  const stops: (() => void)[] = [];
  const process = useProcessStore.getState();
  syncExportJobs(process.exportQueue, process.exportQueueHeld);
  syncAiTasks(process.activeAiTasks);
  stops.push(
    useProcessStore.subscribe((state, previous) => {
      if (state.exportQueue !== previous.exportQueue || state.exportQueueHeld !== previous.exportQueueHeld)
        syncExportJobs(state.exportQueue, state.exportQueueHeld);
      if (state.activeAiTasks !== previous.activeAiTasks) syncAiTasks(state.activeAiTasks);
      if (state.isIndexing !== previous.isIndexing || state.indexingProgress !== previous.indexingProgress) {
        const activity = useActivityStore.getState();
        if (!state.isIndexing) activity.finish('library:indexing');
        else {
          const { current, total } = state.indexingProgress;
          const fields = {
            source: 'app' as const,
            title: i18n.t('activity.indexing'),
            progress: total > 0 ? { done: current ?? 0, total } : undefined,
          };
          if (activity.entries.some((entry) => entry.id === 'library:indexing')) activity.update('library:indexing', fields);
          else activity.start({ id: 'library:indexing', ...fields });
        }
      }
    }),
    useEditorStore.subscribe((state, previous) => {
      if (state.aiDenoiseBusy === previous.aiDenoiseBusy) return;
      const activity = useActivityStore.getState();
      if (state.aiDenoiseBusy) activity.start({ id: 'ai:denoise', source: 'ai', title: i18n.t('activity.ai.denoise') });
      else activity.finish('ai:denoise');
    }),
    onPhotoBatchChanged(syncBatch),
    onPhotoWritesSettled(({ paths, failed }) => syncPhotoWrite(paths, failed)),
  );
  const listeners = [
    listenSafely<string>('ai-model-download-start', (model) => {
      useActivityStore.getState().start({
        id: 'download:ai-model',
        source: 'download',
        title: i18n.t('activity.download.model', { name: model }),
      });
    }),
    listenSafely<DownloadProgress>('ai-model-download-progress', (payload) => {
      const total = payload.batchTotal || payload.total;
      const done = payload.batchTotal ? (payload.batchReceived ?? 0) : payload.received;
      if (total > 0) useActivityStore.getState().update('download:ai-model', { progress: { done, total } });
    }),
    listenSafely('ai-model-download-finish', () => useActivityStore.getState().finish('download:ai-model')),
    listenSafely<{ taskId: string | null; done: number; total: number }>('ai-fill-progress', ({ taskId, done, total }) => {
      if (taskId && total > 0) useActivityStore.getState().update(`ai:task:${taskId}`, { progress: { done, total } });
    }),
  ];
  return () => {
    for (const stop of stops) stop();
    for (const listener of listeners) void listener.then((stop) => stop());
  };
}
