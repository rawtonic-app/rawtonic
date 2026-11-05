import { invoke } from '@tauri-apps/api/core';
import { Invokes } from '../components/ui/AppProperties';
import { registerQuitFlush } from '../utils/quit';

/** How long quitting waits for the saves before it reports them unsaved. */
const FLUSH_TIMEOUT_MS = 10_000;

/** What a library file's saves are called in the quit prompt (`WRITE_NAMES`
 * in `utils/quit.ts`). */
export const ALBUMS_WRITE_NAME = 'albums';
export const PRESETS_WRITE_NAME = 'presets';

/**
 * Saves one library file (`albums.json`, `presets.json`): each snapshot is
 * sent after every earlier one has finished (at once when none is under
 * way). The backend writes the file on a blocking worker, so two snapshots
 * sent together could otherwise be written in the other order and the older
 * one win; in this order the last save decides what is on disk.
 *
 * The queue is registered with the quit (a `QuitFlush`): a change made right
 * before quitting is written before the process ends. `beforeFlush` sends a
 * change the caller still holds back (a debounce's) ahead of it.
 */
export function makeSaveQueue(
  name: string,
  send: (snapshot: unknown) => Promise<void>,
  { beforeFlush }: { beforeFlush?: () => void } = {}
) {
  let inFlight: Promise<void> | null = null;
  /** The last save's snapshot when that save failed; null once a later save succeeded. */
  let unsaved: { snapshot: unknown } | null = null;
  /** The save made last: a queue holds at most one save waiting. */
  let latest = 0;

  const save = (snapshot: unknown): Promise<void> => {
    const saving = ++latest;
    const next = inFlight ? inFlight.then(() => send(snapshot)) : send(snapshot);
    // Saves run one after another, so the last to settle decides what is on disk.
    const settled = next.then(
      () => {
        if (saving === latest) unsaved = null;
      },
      () => {
        if (saving === latest) unsaved = { snapshot };
      }
    );
    inFlight = settled;
    void settled.then(() => {
      if (inFlight === settled) inFlight = null;
    });
    return next;
  };

  const drain = async (first: Promise<void> | null): Promise<string[]> => {
    for (let queued = first; queued; queued = inFlight) await queued;
    // The last save failed: write its snapshot once more.
    if (unsaved) await save(unsaved.snapshot).catch(() => undefined);
    for (let queued = inFlight; queued; queued = inFlight) await queued;
    return unsaved ? [name] : [];
  };

  /** Waits for the queued saves; resolves with what could not be saved, also
   * when the saves did not finish within `timeoutMs`. Nothing is abandoned:
   * a save that finishes later is still kept, the timeout only ends the
   * wait (otherwise a hung save would tie up the next flush for good). */
  const flush = (timeoutMs: number = FLUSH_TIMEOUT_MS): Promise<string[]> => {
    beforeFlush?.();
    // The saves to wait for are those made before this call; the drain reads
    // them before its first `await`.
    const queued = inFlight;
    const drainSaves = drain(queued);
    let timer: ReturnType<typeof setTimeout> | undefined;
    const expired = new Promise<string[]>((resolve) => {
      timer = setTimeout(() => resolve([name]), timeoutMs);
    });
    const clear = () => clearTimeout(timer);
    void drainSaves.then(clear, clear);
    return Promise.race([drainSaves, expired]);
  };

  let unregister: () => void;
  const register = () => {
    unregister = registerQuitFlush(() => flush());
  };
  register();
  /** For tests: a new run, nothing queued, and the flush registered anew. */
  const reset = () => {
    unregister();
    inFlight = null;
    unsaved = null;
    register();
  };
  return { save, flush, reset };
}

/** The app-wide album list (`albums.json`), its saves in order. */
export const albumSaves = makeSaveQueue(ALBUMS_WRITE_NAME, (tree) => invoke<void>(Invokes.SaveAlbums, { tree }));

/** The user's presets (`presets.json`), their saves in order; `beforeFlush`
 * sends the list a debounce still holds back (see `usePresets`). */
export const presetSaves = makeSaveQueue(
  PRESETS_WRITE_NAME,
  (presets) => invoke<void>(Invokes.SavePresets, { presets }),
  { beforeFlush: () => pendingPresets?.() }
);

/** Set by `usePresets`: sends the preset list its debounce waits to write. */
let pendingPresets: (() => void) | null = null;
export function onPresetsBeforeFlush(send: (() => void) | null) {
  pendingPresets = send;
}
