import { swallowDragClick } from '../../utils/swallowClick';
import type { PointerEvent as ReactPointerEvent } from "react";
import i18n from "i18next";
import { toast } from "react-toastify";
import { useLibraryStore } from "../../store/useLibraryStore";
import { createDragGhost } from "./dragGhost";

/** Event a drop target (`data-album-drop="<id>"`) receives when photographs
 * are dropped on it. A row that takes no photographs (an automatic view such
 * as Previous Import) says why in `data-drop-refused="<message>"`: it shows
 * the refused cursor under the drag and that message on the drop. */
export const TILE_DROP_EVENT = "studio-tile-drop";
export type TileDropDetail = { target: string; paths: string[] };

/** Pointer travel before a press on a tile becomes a drag. */
const DRAG_THRESHOLD = 6;

const dropTargetAt = (x: number, y: number) =>
  document.elementFromPoint(x, y)?.closest<HTMLElement>("[data-album-drop],[data-drop-refused]") ?? null;
const REFUSED_CLASS = "is-drop-refused";

/** Drags photographs from the grid onto albums (pointer events; HTML5 drag and
 * drop is left to the window's file drops). A press on a selected tile drags
 * the whole selection. A completed drag swallows the click that follows. */
export function startTileDrag(event: ReactPointerEvent<HTMLElement>, path: string) {
  if (event.button !== 0 || event.shiftKey || event.metaKey || event.ctrlKey || event.altKey) return;
  // Stars and other controls on the tile keep their own presses.
  if ((event.target as Element).closest(".studio-photo-rating")) return;
  const source = event.currentTarget;
  const pointer = event.pointerId;
  const start = { x: event.clientX, y: event.clientY };
  let ghost: ReturnType<typeof createDragGhost> | null = null;
  let paths: string[] = [];
  let target: HTMLElement | null = null;

  const setTarget = (next: HTMLElement | null) => {
    if (next === target) return;
    target?.classList.remove("is-drop-target", REFUSED_CLASS);
    next?.classList.add(next.dataset.dropRefused ? REFUSED_CLASS : "is-drop-target");
    document.body.classList.toggle(REFUSED_CLASS, !!next?.dataset.dropRefused);
    target = next;
  };
  const move = (e: PointerEvent) => {
    if (e.pointerId !== pointer) return;
    if (!ghost) {
      if (Math.hypot(e.clientX - start.x, e.clientY - start.y) < DRAG_THRESHOLD) return;
      const selected = useLibraryStore.getState().multiSelectedPaths;
      paths = selected.includes(path) ? selected : [path];
      // The pressed tile's thumbnail with the number of photographs dragged.
      ghost = createDragGhost({
        label:
          (paths.length === 1 && path.split(/[\\/]/).pop()) ||
          i18n.t("studio.library.drag.photographs", {
            count: paths.length,
            n: paths.length.toLocaleString(),
          }),
        image: source.querySelector("img")?.src,
        count: paths.length,
        kind: "photos",
      });
      source.classList.add("is-drag-source");
      try {
        source.setPointerCapture(pointer);
      } catch {
        // Synthetic pointers cannot be captured; document listeners still follow.
      }
    }
    ghost.move(e.clientX, e.clientY);
    setTarget(dropTargetAt(e.clientX, e.clientY));
  };
  const end = (e: PointerEvent) => {
    if (e.pointerId !== pointer) return;
    document.removeEventListener("pointermove", move, true);
    document.removeEventListener("pointerup", end, true);
    document.removeEventListener("pointercancel", end, true);
    if (!ghost) return;
    ghost.remove();
    source.classList.remove("is-drag-source");
    const drop = e.type === "pointerup" ? dropTargetAt(e.clientX, e.clientY) : null;
    setTarget(null);
    // The press ended a drag, not a click on the tile.
    swallowDragClick();
    if (drop?.dataset.dropRefused) toast.info(drop.dataset.dropRefused);
    else if (drop?.dataset.albumDrop)
      drop.dispatchEvent(
        new CustomEvent<TileDropDetail>(TILE_DROP_EVENT, {
          bubbles: true,
          detail: { target: drop.dataset.albumDrop, paths },
        }),
      );
  };
  document.addEventListener("pointermove", move, true);
  document.addEventListener("pointerup", end, true);
  document.addEventListener("pointercancel", end, true);
}
