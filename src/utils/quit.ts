import { invoke } from '@tauri-apps/api/core';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { Invokes } from '../components/ui/AppProperties';
import { useUIStore } from '../store/useUIStore';
import {
  batchFailures,
  batchOfEntry,
  batchPhotoCount,
  onBatchWriteSettled,
  onPhotoWritesSettled,
  photoBatches,
} from './photoWrites';

/** A write that must reach disk before the app quits (a debounced save):
 * resolves with what it could not save, empty when everything is saved. */
export type QuitFlush = () => Promise<string[]>;

const flushes = new Set<QuitFlush>();

/** Registers a flush for quitting; returns its unregistration. */
export function registerQuitFlush(flush: QuitFlush): () => void {
  flushes.add(flush);
  return () => {
    flushes.delete(flush);
  };
}

/** Runs every registered flush; resolves with what could not be saved. */
export async function flushBeforeQuit(): Promise<string[]> {
  const results = await Promise.allSettled([...flushes].map((flush) => flush()));
  return results.flatMap((result) => (result.status === 'fulfilled' ? result.value : [String(result.reason)]));
}

/** What a write that failed is called in the quit prompt. */
const WRITE_NAMES: Record<string, string> = {
  recipe: 'Edits',
  paste: 'Pasted settings',
  preset: 'Preset',
  style: 'Style',
  reset: 'Reset',
  lens: 'Lens correction',
  auto: 'Auto adjustments',
  faceRetouch: 'Face retouch',
  recipeValues: 'Recipe changes',
  exif: 'EXIF',
  text: 'Text metadata',
  snapshot: 'Snapshot',
};

const fileName = (path: string) => path.split('?vc=')[0].split(/[\\/]/).pop() ?? path;

/** `what` of `file` (`Edits of a.dng`). */
function describe(kind: string, path: string): string {
  // Catalog writes (ratings, labels, flags, keywords) are commands, not paths.
  const name = WRITE_NAMES[kind] ?? (kind.split(/[\\/]/).length > 1 ? 'Write' : kind);
  return `${name} of ${fileName(path)}`;
}

/** A failed write or an unfinished batch, as the prompt lists it. */
type QuitFailure = { order: number; items: string[] };

/** Failures of queued writes during this quit: the last failure of each
 * write is kept (one whose retry succeeded is not), so a report taken
 * before a write was retried still has that write's failure. */
class QuitDrain {
  private readonly identified = new Map<number, { kind: string; failed: string[] }>();
  private readonly unidentified: string[] = [];
  /** A batch heard short of all its photographs written yet (`why`). */
  private readonly batches = new Map<number, string[]>();

  /** The batches running when the quit starts: their failures are this
   * quit's business too (a batch that settled before is not). */
  recordRunningBatches(): void {
    for (const batch of photoBatches().values()) {
      if (batchPhotoCount(batch).done < batch.total) this.batches.set(batch.id, []);
    }
  }

  /** A settled write (`failed`: the photographs it did not write). */
  record(id: number, kind: string, failed: string[]): void {
    if (failed.length === 0) this.identified.delete(id);
    else this.identified.set(id, { kind, failed });
  }

  /** A photograph of batch `id` was not written (`why`), though the batch
   * goes on. */
  recordBatchPhoto(id: number, why: string): void {
    const kept = this.batches.get(id) ?? [];
    if (!kept.includes(why)) kept.push(why);
    this.batches.set(id, kept);
  }

  /** Failures without an identity (a flush's own), kept until the end. */
  recordOther(items: string[]): void {
    if (items.length > 0) this.unidentified.push(...items);
  }

  /** Everything that failed, in submission order. A batch still running is
   * reported as far as it got: its photographs written so far are saved,
   * the others lose their change if the user quits. */
  items(): string[] {
    const identified: QuitFailure[] = [...this.identified].map(([order, { kind, failed }]) => ({
      order,
      items: failed.map((path) => describe(kind, path)),
    }));
    for (const batch of photoBatches().values()) {
      const { done, total } = batchPhotoCount(batch);
      if (done < total) {
        identified.push({
          order: batch.id,
          items: [`${WRITE_NAMES[batch.kind] ?? 'Batch'} not finished (${done} of ${total} photos saved)`],
        });
      }
      const failed = batchFailures(batch);
      if (failed.length > 0) identified.push({ order: batch.id, items: failed });
      const heard = this.batches.get(batch.id);
      if (heard?.length) identified.push({ order: batch.id, items: heard });
    }
    identified.sort((a, b) => a.order - b.order);
    return [...identified.flatMap((failure) => failure.items), ...this.unidentified];
  }
}

/** Runs the registered flushes (the editor's recipe drain awaits every
 * queued photo write, utils/photoWrites) as the quit drain, hearing each
 * photograph a batch command writes (so quitting waits for a long command
 * photo by photo) and each settled write's failure meanwhile. Resolves with
 * what could not be saved (the flushes' failures first), or null when
 * everything is saved. */
async function drainBeforeQuit(): Promise<string[] | null> {
  await invoke(Invokes.QuitSaving).catch(() => undefined);
  const kept = new QuitDrain();
  kept.recordRunningBatches();
  const progress = () => void invoke(Invokes.QuitSaving).catch(() => undefined);
  const stopWrites = onPhotoWritesSettled((settled) => {
    kept.record(settled.id, settled.kind, settled.failed);
    progress();
  });
  const stopBatches = onBatchWriteSettled(({ tag, path, ok, also }) => {
    const batch = [...photoBatches().values()].find((candidate) => candidate.tag === tag);
    if (!batch) return;
    // A failed photograph is kept even though its write may still finish (a
    // retry inside the command reports it written later).
    if (ok !== undefined) kept.recordBatchPhoto(batch.id, `${fileName(path)} (${ok})`);
    for (const extra of also) kept.recordBatchPhoto(batch.id, `${fileName(extra)} (not written with ${fileName(path)})`);
    progress();
  });
  try {
    // The editor's flushes run again until one writes nothing new (its
    // drain drip-feeds the photograph-by-photograph retries); every round's
    // failures are kept.
    let emptied = false;
    while (!emptied) {
      const round = await flushBeforeQuit();
      kept.recordOther(round);
      emptied = round.length === 0;
    }
    const items = kept.items();
    return items.length > 0 ? items : null;
  } finally {
    stopBatches();
    stopWrites();
  }
}

/** Asks the user whether to quit although `failures` were not saved;
 * `quit` quits. */
export type AskToQuitAnyway = (failures: string[], quit: () => void) => void;

/** The app's confirmation dialog for a quit with unsaved edits. */
export const confirmQuitWithUnsaved: AskToQuitAnyway = (failures, quit) => {
  // Quit may come from the menu while the window is minimised or hidden.
  const appWindow = getCurrentWindow();
  void appWindow.unminimize().then(() => appWindow.show()).then(() => appWindow.setFocus()).catch(() => undefined);
  const listed = failures.slice(0, 5).join(', ') + (failures.length > 5 ? `, … (${failures.length} in all)` : '');
  useUIStore.getState().setUI({
    confirmModalState: {
      confirmText: 'Quit Anyway',
      confirmVariant: 'destructive',
      isOpen: true,
      message: `These changes could not be saved: ${listed}. Quitting now loses them; cancel to keep Rawtonic open and try again.`,
      onConfirm: quit,
      title: 'Changes Not Saved',
    },
  });
};

let handlingQuit = false;

/**
 * The backend's `app-quit-requested` (src-tauri/src/app_quit.rs): closing the
 * window or Quit waits here until the pending saves are on disk. If one
 * fails, the app stays open and the user decides; a failed save is never
 * dropped silently: every failed write of the drain counts, a batch's
 * failed or unfinished photographs too, not just the editor's saves. Each
 * settled write, and each photograph a batch writes, is progress: the
 * backend's deadline counts from the last one.
 */
export async function handleQuitRequest(askToQuitAnyway: AskToQuitAnyway): Promise<void> {
  if (handlingQuit) return;
  handlingQuit = true;
  try {
    // The flushes' own failures are kept by the drain; the prompt lists them
    // with every failed write of the drain (also one a flush retried).
    const failures = await drainBeforeQuit();
    if (!failures) {
      await invoke(Invokes.FinishQuit);
      return;
    }
    await invoke(Invokes.CancelQuit);
    askToQuitAnyway(failures, () => {
      invoke(Invokes.FinishQuit).catch((err) => console.error('Failed to quit:', err));
    });
  } finally {
    handlingQuit = false;
  }
}
