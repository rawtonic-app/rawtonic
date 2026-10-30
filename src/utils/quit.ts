import { invoke } from '@tauri-apps/api/core';
import i18n from 'i18next';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { toast } from 'react-toastify';
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

const listed = (failures: string[]) =>
  failures.slice(0, 5).join(', ') + (failures.length > 5 ? `, … (${failures.length} in all)` : '');

/** Registers a flush for quitting; returns its unregistration. */
export function registerQuitFlush(flush: QuitFlush): () => void {
  flushes.add(flush);
  return () => {
    flushes.delete(flush);
  };
}

/** Runs every registered flush; resolves with what could not be saved. */
export async function flushBeforeQuit(): Promise<string[]> {
  const results = await Promise.allSettled([...flushes].map((flush) => Promise.resolve().then(flush)));
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
  /** Batches done before the quit started: their failures were told then,
   * and staying open would save nothing of theirs. */
  private readonly before = new Set<number>();

  /** The batches running when the quit starts: their failures are this
   * quit's business too (a batch that settled before is not). */
  recordRunningBatches(): void {
    for (const batch of photoBatches().values()) {
      if (batchPhotoCount(batch).done < batch.total) this.batches.set(batch.id, []);
      else this.before.add(batch.id);
    }
  }

  /** A settled write (`failed`: the photographs it did not write). */
  record(id: number, kind: string, failed: string[]): void {
    // A whole-command failure can name every photo, including ones whose
    // successful per-photo report already arrived. The batch registry has
    // the precise failures and conservatively marks any unreported photo.
    if (batchOfEntry(id)) return;
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
      if (this.before.has(batch.id)) continue;
      const { done, total } = batchPhotoCount(batch);
      if (done < total) {
        identified.push({
          order: batch.id,
          items: [`${WRITE_NAMES[batch.kind] ?? 'Batch'} not finished (${done} of ${total} photos saved)`],
        });
      }
      // The registry and this drain may both have heard the same report.
      // De-duplicate within this batch, without hiding another write's loss.
      const failed = [...new Set([...batchFailures(batch), ...(this.batches.get(batch.id) ?? [])])];
      if (failed.length > 0) identified.push({ order: batch.id, items: failed });
    }
    identified.sort((a, b) => a.order - b.order);
    return [...identified.flatMap((failure) => failure.items), ...this.unidentified];
  }
}

/** How long a quit saves before it says so (a quick one shows nothing). */
export const QUIT_NOTICE_MS = 500;
export const QUIT_NOTICE_ID = 'quit-saving';

/** The notice that the app saves before it quits: shown once saving takes
 * longer than `QUIT_NOTICE_MS`, with how far the running batches got. */
function savingNotice(): { update(): void; stop(): void } {
  let shown = false;
  const text = () => {
    let done = 0;
    let total = 0;
    for (const batch of photoBatches().values()) {
      const count = batchPhotoCount(batch);
      if (count.done >= count.total) continue;
      done += count.done;
      total += count.total;
    }
    return total > 0 ? i18n.t('notifications.quitSavingPhotos', { done, total }) : i18n.t('notifications.quitSaving');
  };
  const timer = setTimeout(() => {
    shown = true;
    toast.info(text(), { toastId: QUIT_NOTICE_ID, autoClose: false, closeOnClick: false });
  }, QUIT_NOTICE_MS);
  return {
    update: () => {
      if (shown) toast.update(QUIT_NOTICE_ID, { render: text() });
    },
    stop: () => {
      clearTimeout(timer);
      if (shown) toast.dismiss(QUIT_NOTICE_ID);
    },
  };
}

/** Records settled queue writes and the real bridge's per-photo events.
 * Every progress command belongs only to this quit request. */
function hearWrites(kept: QuitDrain, requestId: number, update: () => void): () => void {
  const progress = () => {
    update();
    if (handlingQuit === requestId) void invoke(Invokes.QuitSaving, { requestId }).catch(() => undefined);
  };
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
  return () => {
    stopBatches();
    stopWrites();
  };
}

/** Drains the pending saves once. Each flush (`QuitFlush`) waits for the
 * writes already queued, then writes what it has and returns what it could
 * not save - its own retry; its failures are not pending work, so a flush
 * returning them is not run again (a save that keeps failing must reach the
 * user's decision, never a retry loop). Resolves with what could not be
 * saved (the flushes' failures first), or null when everything is saved. */
async function drainBeforeQuit(requestId: number): Promise<string[] | null> {
  const kept = new QuitDrain();
  kept.recordRunningBatches();
  const notice = savingNotice();
  const stopHearing = hearWrites(kept, requestId, notice.update);
  try {
    if (await invoke<boolean>(Invokes.QuitSaving, { requestId }) !== true) {
      cancelQuitRequest(requestId);
      return null;
    }
    kept.recordOther(await flushBeforeQuit());
    const items = kept.items();
    return items.length > 0 ? items : null;
  } finally {
    stopHearing();
    notice.stop();
  }
}

/** Forgets what a library's pending saves held once a switch to another
 * library begins (its saves are written or discarded by then); returns the
 * end of the switch. */
export type LibrarySwitchReset = () => () => void;

const resets = new Set<LibrarySwitchReset>();

/** Registers a reset for library switches; returns its unregistration. */
export function registerLibrarySwitchReset(reset: LibrarySwitchReset): () => void {
  resets.add(reset);
  return () => {
    resets.delete(reset);
  };
}

/** Saves that failed and are sent again only when the user asks: Retry
 * in the library switch's question. */
const retries = new Set<() => Promise<void>>();

/** Registers what Retry sends again; returns its unregistration. */
export function registerRetryAfterAsking(retry: () => Promise<void>): () => void {
  retries.add(retry);
  return () => {
    retries.delete(retry);
  };
}

/** What the user chose when saves failed before a library switch. */
export type UnsavedChoice = 'retry' | 'discard' | 'cancel';
export type AskBeforeSwitch = (failures: string[]) => Promise<UnsavedChoice>;

/** The app's dialog for a library switch with unsaved edits. */
export const confirmSwitchWithUnsaved: AskBeforeSwitch = (failures) =>
  new Promise((resolve) => {
    const t = i18n.t.bind(i18n);
    useUIStore.getState().setUI({
      confirmModalState: {
        isOpen: true,
        title: t('session.unsaved.title'),
        message: t('session.unsaved.message', { files: listed(failures) }),
        confirmText: t('session.unsaved.retry'),
        secondaryText: t('session.unsaved.discard'),
        secondaryVariant: 'destructive',
        cancelText: t('session.unsaved.cancel'),
        onConfirm: () => resolve('retry'),
        onSecondary: () => resolve('discard'),
        onCancel: () => resolve('cancel'),
      },
    });
  });

/**
 * Switching library (opening or creating a session or catalog) is a
 * barrier: every pending save is written to the library it was made in
 * first (the same flushes as quitting). While one still fails the switch
 * waits for the user: Retry, Discard the unsaved edits, or Cancel (null,
 * nothing opened). Once past it, nothing pending from the old library is
 * written through the new one (`registerLibrarySwitchReset`).
 */
export async function switchLibraryAfterSaves<T>(
  open: () => Promise<T>,
  ask: AskBeforeSwitch = confirmSwitchWithUnsaved,
): Promise<T | null> {
  // From the first save until the next library is open nothing can be
  // edited: an edit made while the saves are written could miss them, and
  // one made later could be saved in neither library. While the user is
  // asked, the question holds the app instead.
  const hold = (holding: boolean) => useUIStore.getState().setUI({ switchingLibrary: holding });
  hold(true);
  let ends: (() => void)[] = [];
  try {
    for (;;) {
      const failures = await flushBeforeQuit();
      if (!failures.length) break;
      hold(false);
      const choice = await ask(failures);
      if (choice === 'cancel') return null;
      hold(true);
      if (choice === 'discard') break;
      await Promise.all([...retries].map((retry) => retry()));
    }
    ends = [...resets].map((reset) => reset());
    return await open();
  } finally {
    ends.forEach((end) => end());
    hold(false);
  }
}

/** Asks the user whether to quit although `failures` were not saved;
 * `quit` quits. */
export type AskToQuitAnyway = (failures: string[], quit: () => void, cancel: () => void) => void;

/** The app's confirmation dialog for a quit with unsaved edits. */
export const confirmQuitWithUnsaved: AskToQuitAnyway = (failures, quit, cancel) => {
  let decided = false;
  const decide = (action: () => void) => {
    if (decided) return;
    decided = true;
    action();
  };
  // Quit may come from the menu while the window is minimised or hidden.
  const appWindow = getCurrentWindow();
  void appWindow.unminimize().then(() => appWindow.show()).then(() => appWindow.setFocus()).catch(() => undefined);
  useUIStore.getState().setUI({
    confirmModalState: {
      confirmText: i18n.t('modals.quit.quitAnyway'),
      confirmVariant: 'destructive',
      isOpen: true,
      message: i18n.t('modals.quit.unsavedMessage', { changes: listed(failures) }),
      onConfirm: () => decide(quit),
      onCancel: () => decide(cancel),
      title: i18n.t('modals.quit.unsavedTitle'),
    },
  });
};

let handlingQuit: number | null = null;
let latestQuitRequest: number | null = null;

/** A native deadline cancelled this request. A stale event cannot clear
 * a newer request's guard; disk work already submitted keeps running. */
export function cancelQuitRequest(requestId: number): boolean {
  if (!Number.isSafeInteger(requestId) || requestId <= 0 || latestQuitRequest !== requestId) return false;
  latestQuitRequest = null;
  if (handlingQuit === requestId) handlingQuit = null;
  return true;
}

/** Saves for one native request. Only that request may finish, and only
 * explicit confirmation may force a quit after failed saves. */
export async function handleQuitRequest(askToQuitAnyway: AskToQuitAnyway, requestId: number): Promise<void> {
  if (!Number.isSafeInteger(requestId) || requestId <= 0 || handlingQuit !== null) return;
  handlingQuit = requestId;
  latestQuitRequest = requestId;
  const request = { requestId };
  try {
    const failures = await drainBeforeQuit(requestId);
    if (handlingQuit !== requestId) return;
    if (!failures) {
      await invoke(Invokes.FinishQuit, { ...request, force: false });
      return;
    }
    const allowed = await invoke<boolean>(Invokes.CancelQuit, { ...request, allowQuitAnyway: true });
    if (allowed !== true || handlingQuit !== requestId) return;
    let decided = false;
    const decide = (command: Invokes, args: Record<string, unknown>) => {
      if (decided || latestQuitRequest !== requestId) return;
      decided = true;
      latestQuitRequest = null;
      void invoke(command, { ...request, ...args }).catch((error) => console.error('Quit decision failed:', error));
    };
    askToQuitAnyway(
      failures,
      () => decide(Invokes.FinishQuit, { force: true }),
      () => decide(Invokes.CancelQuit, { allowQuitAnyway: false }),
    );
  } catch (error) {
    // Logging alone leaves the native timeout armed. Cancel it explicitly;
    // the backend rejects this command if a newer request already owns it.
    await invoke(Invokes.CancelQuit, { ...request, allowQuitAnyway: false })
      .catch((cancelError) => console.error('Cancelling the quit failed:', cancelError));
    cancelQuitRequest(requestId);
    console.error('Saving before quitting failed:', error);
  } finally {
    if (handlingQuit === requestId) handlingQuit = null;
  }
}
