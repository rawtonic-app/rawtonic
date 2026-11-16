import { Invokes } from '../components/ui/AppProperties';
import { useCallback, useMemo, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';

// Submit the first visible range immediately. While IPC is outstanding, keep
// only the newest range; scrolling cannot accumulate obsolete thumbnail jobs.
export function createThumbnailQueue(send: (paths: string[]) => Promise<unknown>) {
  const generated = new Set<string>();
  let visible: string[] = [];
  let inView = new Set<string>();
  let dirty = false;
  let sending = false;
  let failed = false;

  const flush = () => {
    if (sending || !dirty) return;
    sending = true;
    dirty = false;
    failed = false;
    const paths = visible.filter((path) => !generated.has(path));
    void send(paths)
      .catch((error) => {
        failed = true;
        console.error('Failed to update thumbnail queue:', error);
      })
      .finally(() => {
        sending = false;
        flush();
      });
  };
  return {
    request(paths: string[]) {
      if (!failed && visible.length === paths.length && visible.every((path, i) => path === paths[i])) return;
      visible = paths;
      inView = new Set(paths);
      dirty = true;
      flush();
    },
    markGenerated(path: string) {
      generated.add(path);
    },
    /** Whether the viewport asked for this path last (a tile in view). */
    isInView(path: string) {
      return inView.has(path);
    },
    clear() {
      generated.clear();
      visible = [];
      inView = new Set();
      dirty = true;
      flush();
    }
  };
}

export function useThumbnails() {
  const queue = useMemo(() => createThumbnailQueue((paths) => invoke(Invokes.UpdateThumbnailQueue, { paths })), []);
  const requestThumbnails = useCallback((paths: string[]) => queue.request(paths), [queue]);
  const markGenerated = useCallback((path: string) => queue.markGenerated(path), [queue]);
  const clearThumbnailQueue = useCallback(() => queue.clear(), [queue]);
  const isInView = useCallback((path: string) => queue.isInView(path), [queue]);
  useEffect(() => () => queue.clear(), [queue]);
  return { requestThumbnails, clearThumbnailQueue, markGenerated, isInView };
}
