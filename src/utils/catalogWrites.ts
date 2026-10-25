import { CATALOG_CHANGED, FILE_OPERATIONS_CHANGED, catalogChanged } from './catalogEvents';
import { invoke, type InvokeArgs } from '@tauri-apps/api/core';
import { IMPORTS_CHANGED } from './folderSync';
import { OFFLINE_CHANGED } from './offline';

let last: Promise<unknown> = Promise.resolve();
/** Photographs this window rated, with how many of those writes did not
 * fail (yet). Labels and keywords keep the stored rating; a failed write
 * changed nothing. */
const rated = new Map<string, number>();

/** Whether this window rated `path` (`invokeInOrder`): an announcement
 * from the background (a camera's rating the facts pass took) may be
 * older than that rating and must not replace it. */
export function metadataWritten(path: string): boolean {
  return (rated.get(path) ?? 0) > 0;
}

/**
 * Invokes a rating, colour label or keyword write after the ones given
 * before it have finished. These commands run on blocking workers (the
 * catalog may be locked for seconds; the main thread must not wait), so two
 * quick key presses would otherwise race and the older value could land
 * last. A failed write does not stop the ones after it.
 */
export function invokeInOrder<T>(command: string, args: InvokeArgs): Promise<T> {
  const paths = (args as { paths?: unknown }).paths;
  const ratedPaths =
    command === 'set_rating_for_paths' && Array.isArray(paths)
      ? paths.filter((path): path is string => typeof path === 'string')
      : [];
  for (const path of ratedPaths) rated.set(path, (rated.get(path) ?? 0) + 1);
  const run = last.then(() => invoke<T>(command, args));
  run.catch((error: unknown) => {
    // A save that failed for some photographs names them; the others were
    // rated.
    const failed = (error as { failed?: unknown } | null)?.failed;
    const undone = Array.isArray(failed) ? ratedPaths.filter((path) => failed.includes(path)) : ratedPaths;
    for (const path of undone) {
      const left = (rated.get(path) ?? 1) - 1;
      if (left > 0) rated.set(path, left);
      else rated.delete(path);
    }
  });
  last = run.catch(() => undefined);
  void run.finally(catalogChanged).catch(() => undefined);
  return run;
}

export { CATALOG_CHANGED, catalogChanged } from './catalogEvents';

/** Fired after the photographs' context menu changed a session album, so
 * the Albums tool shows the new counts and content (re-exported by
 * StudioAlbums, where it began). */
export const SESSION_ALBUMS_CHANGED = 'rawtonic:session-albums-changed';

/** What changes what the catalog finds: a write (`CATALOG_CHANGED`), an
 * import, tethered capture or synchronization (`IMPORTS_CHANGED`), a relink
 * or a check that found files gone (`OFFLINE_CHANGED`), album membership
 * (`SESSION_ALBUMS_CHANGED`, album rules), file operations and undo
 * (`FILE_OPERATIONS_CHANGED`). */
export const CATALOG_EVENTS = [CATALOG_CHANGED, IMPORTS_CHANGED, OFFLINE_CHANGED, SESSION_ALBUMS_CHANGED, FILE_OPERATIONS_CHANGED];

/** Calls `handler` after any of `events` (default: everything that changes
 * what the catalog finds). Returns the unsubscribe. */
export function onCatalogChange(handler: () => void, events = CATALOG_EVENTS): () => void {
  for (const event of events) window.addEventListener(event, handler);
  return () => {
    for (const event of events) window.removeEventListener(event, handler);
  };
}
