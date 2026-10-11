import { invoke } from '@tauri-apps/api/core';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { Invokes } from '../components/ui/AppProperties';
import { useUIStore } from '../store/useUIStore';

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
