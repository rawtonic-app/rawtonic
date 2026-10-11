import { useEffect, useRef } from "react";
import { isTauri } from "@tauri-apps/api/core";
import { getCurrentWebview } from "@tauri-apps/api/webview";

/** Files and folders dragged from Finder / Explorer onto the window open the
 * import dialog with them (never a silent copy). Dropped onto an album of
 * the sidebar they are added to it after the import; onto a folder of the
 * library they are copied into it. Targets mark themselves with
 * `data-album-drop="<id>"` (+ `data-album-name`) or `data-folder-drop="<path>"`. */
export type FileDropTarget =
  | { kind: "album"; id: string; name: string }
  | { kind: "folder"; path: string }
  | null;

export interface FileDrop {
  paths: string[];
  target: FileDropTarget;
}

const TARGETS = "[data-album-drop],[data-folder-drop]";
/** The class a target under a dragged file gets (as for grid tiles dragged onto albums). */
export const DROP_TARGET_CLASS = "is-drop-target";
/** On `<body>` while files are dragged over the window. */
export const FILE_DRAG_CLASS = "is-file-drag";

/** The drop target `element` is in, if any. */
export function dropTargetOf(
  element: Element | null,
): { element: HTMLElement; target: FileDropTarget } | null {
  const found = element?.closest<HTMLElement>(TARGETS);
  if (!found) return null;
  if (found.dataset.albumDrop)
    return {
      element: found,
      target: {
        kind: "album",
        id: found.dataset.albumDrop,
        name: found.dataset.albumName ?? found.textContent?.trim() ?? "",
      },
    };
  if (found.dataset.folderDrop)
    return {
      element: found,
      target: { kind: "folder", path: found.dataset.folderDrop },
    };
  return null;
}

/** A drag position of the webview (physical pixels) in CSS pixels. */
export function toCssPoint(
  position: { x: number; y: number },
  scale = window.devicePixelRatio || 1,
) {
  return { x: position.x / scale, y: position.y / scale };
}

const separator = /[\\/]/;
/** Library folders that hold no photographs of their own (as `ingest::RESERVED_FOLDERS`). */
const RESERVED = new Set(["Trash", "masks", "previews", "smart-previews"]);
/** Whether photographs dropped onto `folder` may be copied into it: a folder
 * inside the open library (not the library folder itself, its Trash, the
 * app's own folders or a hidden folder); the backend checks the same (`ingest::destination_folder`). */
export function folderAcceptsDrop(
  folder: string,
  library: string | null | undefined,
): boolean {
  if (!library) return false;
  const root = library.replace(/[\\/]+$/, "");
  if (!folder.startsWith(root) || !separator.test(folder.charAt(root.length)))
    return false;
  const first = folder.slice(root.length + 1).split(separator)[0];
  return Boolean(first) && !RESERVED.has(first) && !first.startsWith(".");
}

/** Follows files dragged over the window (Tauri's webview drag-and-drop
 * events): `<body>` gets `FILE_DRAG_CLASS` while they are over it, the
 * album or folder under the pointer `DROP_TARGET_CLASS`, and `onDrop` the
 * dropped paths with that target. `onDrag` tells when a drag starts and ends. */
export function useWindowFileDrop(
  onDrop: (drop: FileDrop) => void,
  onDrag?: (dragging: boolean) => void,
) {
  const handlers = useRef({ onDrop, onDrag });
  handlers.current = { onDrop, onDrag };
  useEffect(() => {
    if (!isTauri()) return;
    let disposed = false;
    let stop: (() => void) | undefined;
    let highlighted: HTMLElement | null = null;
    const highlight = (next: HTMLElement | null) => {
      if (next === highlighted) return;
      highlighted?.classList.remove(DROP_TARGET_CLASS);
      next?.classList.add(DROP_TARGET_CLASS);
      highlighted = next;
    };
    const at = (position: { x: number; y: number }) => {
      const point = toCssPoint(position);
      return dropTargetOf(document.elementFromPoint(point.x, point.y));
    };
    const end = () => {
      highlight(null);
      document.body.classList.remove(FILE_DRAG_CLASS);
      handlers.current.onDrag?.(false);
    };
    void getCurrentWebview()
      .onDragDropEvent(({ payload }) => {
        if (payload.type === "enter") {
          document.body.classList.add(FILE_DRAG_CLASS);
          handlers.current.onDrag?.(true);
          highlight(at(payload.position)?.element ?? null);
        } else if (payload.type === "over") {
          highlight(at(payload.position)?.element ?? null);
        } else if (payload.type === "drop") {
          const target = at(payload.position)?.target ?? null;
          end();
          if (payload.paths.length)
            handlers.current.onDrop({ paths: payload.paths, target });
        } else end();
      })
      .then((fn) => {
        if (disposed) fn();
        else stop = fn;
      })
      .catch(() => {});
    return () => {
      disposed = true;
      stop?.();
      end();
    };
  }, []);
}
