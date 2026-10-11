import { useCallback, useMemo, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";

// Submit the first visible range immediately. While IPC is outstanding, keep
// only the newest range; scrolling cannot accumulate obsolete thumbnail jobs.
export function createThumbnailQueue(send: (paths: string[]) => Promise<unknown>) {
  const generated = new Set<string>();
  let visible: string[] = [];
  let dirty = false;
  let sending = false;
  let failed = false;

  const flush = () => {
    if (sending || !dirty) return;
    sending = true;
    dirty = false;
    failed = false;
    const paths = visible.filter((path) => !generated.has(path));
    void send(paths).catch((error) => {
      failed = true;
      console.error("Failed to update thumbnail queue:", error);
    }).finally(() => {
      sending = false;
      flush();
    });
  };
  return {
    request(paths: string[]) {
      if (!failed && visible.length === paths.length && visible.every((path, i) => path === paths[i])) return;
      visible = paths;
      dirty = true;
      flush();
    },
    markGenerated(path: string) { generated.add(path); },
    clear() {
      generated.clear();
      visible = [];
      dirty = true;
      flush();
    },
  };
}

export function useThumbnails() {
  const queue = useMemo(() => createThumbnailQueue((paths) => invoke("update_thumbnail_queue", { paths })), []);
  const requestThumbnails = useCallback((paths: string[]) => queue.request(paths), [queue]);
  const markGenerated = useCallback((path: string) => queue.markGenerated(path), [queue]);
  const clearThumbnailQueue = useCallback(() => queue.clear(), [queue]);
  useEffect(() => () => queue.clear(), [queue]);
  return { requestThumbnails, clearThumbnailQueue, markGenerated };
}
