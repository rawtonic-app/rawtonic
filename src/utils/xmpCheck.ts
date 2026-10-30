import { invoke } from '@tauri-apps/api/core';
import { Invokes } from '../components/ui/AppProperties';
import { useLibraryStore } from '../store/useLibraryStore';
import { throttleAsync } from './throttle';

/** A folder of the file system, not a view (an album, Missing Photographs…). */
const isFolder = (path: string | null): path is string => !!path && /^(\/|[A-Za-z]:[\\/]|\\\\)/.test(path);

/**
 * Finds XMP sidecars another program changed while they were in sync with
 * the library (`session_xmp_check`), without waiting for a write: the whole
 * library when it opens and whenever the app is back in front, a folder when
 * it is shown. No polling: the platform crates offer no file watching, and
 * an unchanged sidecar is only looked at, not read. `found` runs when some
 * were marked, so the session bar's XMP status shows them. Returns a stop.
 */
export function watchXmpSidecars(directory: string, found: () => void): () => void {
  let stopped = false;
  const check = async (folder: string | null) => {
    const marked = await invoke<number>(Invokes.SessionXmpCheck, { directory, folder });
    if (!stopped && marked > 0 && useLibraryStore.getState().sessionDirectory === directory) found();
  };
  const library = throttleAsync(() => check(null), 1000);
  let folder: string | null = null;
  const shown = throttleAsync(() => (isFolder(folder) ? check(folder) : undefined), 1000);
  const back = () => {
    if (document.visibilityState !== 'hidden') library();
  };
  window.addEventListener('focus', back);
  document.addEventListener('visibilitychange', back);
  let current = useLibraryStore.getState().currentFolderPath;
  const unsubscribe = useLibraryStore.subscribe((state) => {
    if (state.currentFolderPath === current) return;
    current = state.currentFolderPath;
    if (!isFolder(current)) return;
    folder = current;
    shown();
  });
  library();
  return () => {
    stopped = true;
    library.cancel();
    shown.cancel();
    window.removeEventListener('focus', back);
    document.removeEventListener('visibilitychange', back);
    unsubscribe();
  };
}
