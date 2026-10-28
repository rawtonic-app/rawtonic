import { useLibraryStore } from "../../store/useLibraryStore";


/** Whether Tab belongs to the panel shortcut rather than to focus traversal:
 * with nothing focused, or on the photograph viewer and the browser tiles
 * (`data-panel-shortcut`), never on a field, button, disclosure or separator. */
export function ownsPanelShortcut(target: EventTarget | null) {
  if (
    target === null ||
    target === document.body ||
    target === document.documentElement
  )
    return true;
  return target instanceof HTMLElement && target.dataset.panelShortcut === "true";
}

/** Whether the focus is on a photograph tile of the Library grid or the
 * filmstrip (Space opens the loupe from it, however the tile got the focus). */
export function isPhotoTile(target: EventTarget | null) {
  return target instanceof HTMLElement && target.classList.contains("studio-photo-open");
}

/** The library column's tool group with `title`, when it is on screen. */
export function libraryTool(title: string) {
  return document.querySelector<HTMLDetailsElement>(
    `.studio-navigation details[data-tool-title="${title}"]`,
  );
}

/** Opens a library tool (Metadata, Folders), scrolls it into view and focuses
 * its header; false when the library tools are not shown. */
export function revealLibraryTool(title: string) {
  const group = libraryTool(title);
  if (!group) return false;
  // The toggle event remembers it open (useToolOpen).
  group.open = true;
  group.scrollIntoView?.({ block: "nearest" });
  group.querySelector<HTMLElement>("summary")?.focus({ preventScroll: true });
  return true;
}

/** WebKit on macOS does not focus clicked buttons by default. */
export function focusWorkspaceControl(target: EventTarget | null) {
  if (!(target instanceof Element)) return;
  const control = target.closest('button,summary,[role="separator"]');
  if (control instanceof HTMLElement && !control.matches(":disabled")) {
    control.focus({ preventScroll: true });
  }
}

/** A click on the Library grid's empty space: nothing selected (Lightroom,
 * Capture One). The photograph open in Develop stays open and the keyboard
 * focus stays where it was, so Develop (D) shows the last primary again. */
export function clearLibrarySelection() {
  useLibraryStore.getState().setLibrary({ multiSelectedPaths: [], selectionAnchorPath: null });
}

/** Develop opens on `path` (the photograph open last); an emptied selection
 * selects it again so Develop's tools and the filmstrip agree. */
export function reselectForDevelop(path: string) {
  const library = useLibraryStore.getState();
  if (library.multiSelectedPaths.length > 0) return;
  library.setLibrary({ multiSelectedPaths: [path], libraryActivePath: path, selectionAnchorPath: path });
}
