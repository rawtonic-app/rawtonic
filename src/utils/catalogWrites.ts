import { invoke, type InvokeArgs } from '@tauri-apps/api/core';
import { IMPORTS_CHANGED } from './folderSync';
import { OFFLINE_CHANGED } from './offline';

let last: Promise<unknown> = Promise.resolve();

/**
 * Invokes a rating, colour label or keyword write after the ones given
 * before it have finished. These commands run on blocking workers (the
 * catalog may be locked for seconds; the main thread must not wait), so two
 * quick key presses would otherwise race and the older value could land
 * last. A failed write does not stop the ones after it.
 */
export function invokeInOrder<T>(command: string, args: InvokeArgs): Promise<T> {
  const run = last.then(() => invoke<T>(command, args));
  last = run.catch(() => undefined);
  void run.finally(catalogChanged).catch(() => undefined);
  return run;
}

/** Fired (on `window`) after a write to the library's catalog settled, so
 * searches, value lists and smart albums show what it holds now. */
export const CATALOG_CHANGED = 'rawtonic:catalog-changed';

export function catalogChanged() {
  // A write may settle after the page (or a test's document) is gone.
  if (typeof window !== 'undefined') window.dispatchEvent(new Event(CATALOG_CHANGED));
}

/** Fired after the photographs' context menu changed a session album, so
 * the Albums tool shows the new counts and content (re-exported by
 * StudioAlbums, where it began). */
export const SESSION_ALBUMS_CHANGED = 'rawtonic:session-albums-changed';

/** What changes what the catalog finds: a write (`CATALOG_CHANGED`), an
 * import, tethered capture or synchronization (`IMPORTS_CHANGED`), a relink
 * or a check that found files gone (`OFFLINE_CHANGED`), album membership
 * (`SESSION_ALBUMS_CHANGED`, album rules). */
export const CATALOG_EVENTS = [CATALOG_CHANGED, IMPORTS_CHANGED, OFFLINE_CHANGED, SESSION_ALBUMS_CHANGED];

/** Calls `handler` after any of `events` (default: everything that changes
 * what the catalog finds). Returns the unsubscribe. */
export function onCatalogChange(handler: () => void, events = CATALOG_EVENTS): () => void {
  for (const event of events) window.addEventListener(event, handler);
  return () => {
    for (const event of events) window.removeEventListener(event, handler);
  };
}
