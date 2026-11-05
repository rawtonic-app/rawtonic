import { invoke } from '@tauri-apps/api/core';
import { Invokes } from '../components/ui/AppProperties';

/** How long quitting, Save & Relaunch and a library switch wait for the
 * backend to write what they sent before they report it unsaved. */
const FLUSH_TIMEOUT_MS = 10_000;

/** What the library files' unwritten changes are called in the quit prompt
 * (`WRITE_NAMES` in `utils/quit.ts`): the backend's own failures come with
 * their names. */
export const ALBUMS_WRITE_NAME = 'albums';
export const PRESETS_WRITE_NAME = 'presets';
/** The preset list could not be read: its changes stay in the app (the file
 * is never written over) until a later read succeeds (`WRITE_NAMES` names
 * it in the quit prompt). */
export const PRESETS_UNREAD_NAME = 'presetsUnread';
/** The backend did not answer the flush at all (a change nothing can count
 * then): the quit prompt asks before quitting on it. */
export const UNREPORTED_WRITE_NAME = 'a library change';

/** One ordered queue for every change of `albums.json` and `presets.json`:
 * whole-list saves, the single changes only the backend can apply to the
 * stored list (a photograph added to an album from a menu) and the preset
 * imports. The changes are sent one after another, and each of those
 * commands makes its one read-modify-write before it answers, so a change
 * always starts from the list the earlier ones left behind - the read, the
 * change and the save are never split.
 *
 * The queue is registered with quitting (a `QuitFlush`): quitting, closing
 * the window, Save & Relaunch and a library switch send one `library_flush`
 * after every change sent so far and wait for it; the backend reports what
 * it could not write, including a change that was still running when it
 * waited long enough, and an import that was still reading its files.
 * `beforeFlush` sends a change the caller still holds back (a debounce's)
 * ahead of the flush.
 */
export function makeLibraryQueue({
  beforeFlush,
  flushBackend = true
}: { beforeFlush?: () => void; flushBackend?: boolean } = {}) {
  /** The change sent last; the next one is sent when it settled. */
  let inFlight: Promise<unknown> | null = null;

  /** Sends a change once the earlier one settled (failed or not). */
  const enqueue = <T>(send: () => Promise<T>): Promise<T> => {
    const sent = inFlight ? inFlight.then(send, send) : send();
    const settled = sent.then(
      () => undefined,
      () => undefined
    );
    inFlight = settled;
    void settled.then(() => {
      if (inFlight === settled) inFlight = null;
    });
    return sent;
  };

  /** Sends the changes made before this call, then waits for the backend to
   * write them; resolves with what is not written. Nothing is abandoned: a
   * change that finishes later is still kept, the timeout only ends the wait
   * (otherwise a hung backend would hold up the next flush for good). */
  const flush = (timeoutMs: number = FLUSH_TIMEOUT_MS): Promise<string[]> => {
    beforeFlush?.();
    const drain = (async () => {
      for (let queued = inFlight; queued; queued = inFlight) await queued;
      // The changes were applied by the backend as each one was sent, so
      // they are written; the backend also names a change that failed and a
      // preset import still reading its files.
      if (!flushBackend) return [];
      const failures = await invoke<string[] | null>(Invokes.LibraryFlush, {});
      return failures ?? [];
    })().catch((error) => {
      console.error('Could not flush the album and preset changes:', error);
      return [UNREPORTED_WRITE_NAME];
    });
    let timer: ReturnType<typeof setTimeout> | undefined;
    const expired = new Promise<string[]>((resolve) => {
      timer = setTimeout(() => resolve([UNREPORTED_WRITE_NAME]), timeoutMs);
    });
    const clear = () => clearTimeout(timer);
    void drain.then(clear, clear);
    return Promise.race([drain, expired]).then((failures) => [...failures, ...unwrittenPresets()]);
  };

  /** For tests: a new run, nothing queued. */
  const reset = () => {
    inFlight = null;
  };
  return { enqueue, flush, reset };
}

export type LibraryQueue = ReturnType<typeof makeLibraryQueue>;

/** The app's album list and the user's presets: one queue for both, as the
 * backend has one order. `utils/appWrites.ts` registers its flush with
 * quitting (this module must not import `utils/quit.ts`: the settings store
 * imports this one, and quitting imports the settings store's flush);
 * `beforeFlush` sends the preset list a debounce still holds back (see
 * `usePresets`). */
export const libraryChanges = makeLibraryQueue({ beforeFlush: () => pendingPresets?.() });

/** Saves the album list (`albums.json`), in order. */
export const saveAlbums = (tree: unknown) => libraryChanges.enqueue(() => invoke<void>(Invokes.SaveAlbums, { tree }));

/** Saves the user's presets (`presets.json`), in order. */
export const savePresets = (presets: unknown) =>
  libraryChanges.enqueue(() => invoke<void>(Invokes.SavePresets, { presets }));

/** Set by `usePresets`: the preset list could not be read, so the changes in
 * it are held unsaved (the file is never written over). */
let presetsUnread = false;
export function setPresetsUnread(unread: boolean) {
  presetsUnread = unread;
}

/** The changes held unsaved because their file could not be read (so a
 * later flush must ask about them). */
export function unwrittenPresets(): string[] {
  return presetsUnread ? [PRESETS_UNREAD_NAME] : [];
}

/** Set by `usePresets`: sends the preset list its debounce waits to write. */
let pendingPresets: (() => void) | null = null;
export function onPresetsBeforeFlush(send: (() => void) | null) {
  pendingPresets = send;
}
