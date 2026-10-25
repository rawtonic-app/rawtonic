import { invoke } from '@tauri-apps/api/core';
import i18next from 'i18next';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { Invokes } from '../components/ui/AppProperties';
import { useUIStore } from '../store/useUIStore';

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
  const results = await Promise.allSettled([...flushes].map((flush) => flush()));
  return results.flatMap((result) => (result.status === 'fulfilled' ? result.value : [String(result.reason)]));
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

/** What the user chose when saves failed before a library switch. */
export type UnsavedChoice = 'retry' | 'discard' | 'cancel';
export type AskBeforeSwitch = (failures: string[]) => Promise<UnsavedChoice>;

/** The app's dialog for a library switch with unsaved edits. */
export const confirmSwitchWithUnsaved: AskBeforeSwitch = (failures) =>
  new Promise((resolve) => {
    const t = i18next.t.bind(i18next);
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
  for (;;) {
    const failures = await flushBeforeQuit();
    if (!failures.length) break;
    const choice = await ask(failures);
    if (choice === 'cancel') return null;
    if (choice === 'discard') break;
  }
  const ends = [...resets].map((reset) => reset());
  try {
    return await open();
  } finally {
    ends.forEach((end) => end());
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
  useUIStore.getState().setUI({
    confirmModalState: {
      confirmText: 'Quit Anyway',
      confirmVariant: 'destructive',
      isOpen: true,
      message: `These changes could not be saved: ${listed(failures)}. Quitting now loses them; cancel to keep Rawtonic open and try again.`,
      onConfirm: quit,
      title: 'Changes Not Saved',
    },
  });
};

let handlingQuit = false;

/**
 * The backend's `app-quit-requested` (src-tauri/src/app_quit.rs): closing the
 * window or Quit waits here until the pending saves are on disk. If one
 * fails the app stays open and the user decides; a failed save is never
 * dropped silently.
 */
export async function handleQuitRequest(askToQuitAnyway: AskToQuitAnyway): Promise<void> {
  if (handlingQuit) return;
  handlingQuit = true;
  try {
    await invoke(Invokes.QuitSaving).catch(() => undefined);
    const failures = await flushBeforeQuit();
    if (failures.length === 0) {
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
