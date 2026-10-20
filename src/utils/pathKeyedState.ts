import { invoke } from '@tauri-apps/api/core';
import { useEditorStore } from '../store/useEditorStore';
import { useLibraryStore } from '../store/useLibraryStore';
import { useProcessStore } from '../store/useProcessStore';
import { useSettingsStore } from '../store/useSettingsStore';
import type { AppSettings } from '../components/ui/AppProperties';

const VIRTUAL_COPY = '?vc=';
const basePath = (path: string) => path.split(VIRTUAL_COPY)[0];

/** A photograph or folder now at another place (`PathMove` in
 * src-tauri/src/file_operations.rs). */
export interface PathMove {
  from: string;
  to: string;
}

/** Where `path` is after `moves` (photograph or folder → new place): a
 * moved photograph's virtual copies (`?vc=`) move with it, everything in a
 * moved folder moves with the folder; others stay. */
export function movedPath(path: string, moves: Map<string, string>): string {
  const at = path.indexOf(VIRTUAL_COPY);
  const base = at < 0 ? path : path.slice(0, at);
  const copy = at < 0 ? '' : path.slice(at);
  const to = moves.get(base);
  if (to !== undefined) return to + copy;
  // A folder above it moved.
  let cut = base.length;
  while (cut > 0) {
    cut = Math.max(base.lastIndexOf('/', cut - 1), base.lastIndexOf('\\', cut - 1));
    if (cut <= 0) break;
    const folder = moves.get(base.slice(0, cut));
    if (folder !== undefined) return folder + base.slice(cut) + copy;
  }
  return path;
}

/** `record` with its keys at their new places; a moved key wins over a stale
 * entry already at its new place. */
export function remapPathKeys<T>(record: Record<string, T>, moves: Map<string, string>): Record<string, T> {
  const next: Record<string, T> = {};
  const moved = new Set<string>();
  for (const [key, value] of Object.entries(record)) {
    const to = movedPath(key, moves);
    if (to !== key) {
      next[to] = value;
      moved.add(to);
    } else if (!moved.has(key)) {
      next[key] = value;
    }
  }
  return moved.size ? next : record;
}

/** `paths` at their new places, each once; the same array when none moved. */
function remapPaths(paths: string[], moves: Map<string, string>): string[] {
  const next = paths.map((path) => movedPath(path, moves));
  return next.some((path, i) => path !== paths[i]) ? [...new Set(next)] : paths;
}

/**
 * After a rename or move, or the undo of one (`moves` in the direction just
 * applied): the path-keyed state follows at once, before the list is read
 * again — picks and rejects, ratings, thumbnails and previews, the
 * selection, its anchor and the active photograph, the shown, expanded,
 * pinned and root folders. Virtual copies follow their photograph, and
 * everything in a moved folder the folder. Returns where the photograph
 * open in the editor is now when it moved (the caller opens it there), else
 * null.
 */
export function followPathMoves(pathMoves: PathMove[]): { openPhoto: string | null } {
  const moves = new Map<string, string>();
  for (const { from, to } of pathMoves) {
    if (basePath(from) !== basePath(to)) moves.set(basePath(from), basePath(to));
  }
  if (!moves.size) return { openPhoto: null };
  const move = (path: string | null) => path && movedPath(path, moves);
  useLibraryStore.getState().setLibrary((state) => {
    const expanded = [...state.expandedFolders].map((path) => movedPath(path, moves));
    const expandedChanged = expanded.some((path) => !state.expandedFolders.has(path));
    return {
      imageFlags: remapPathKeys(state.imageFlags, moves),
      imageRatings: remapPathKeys(state.imageRatings, moves),
      selectionAnchorPath: move(state.selectionAnchorPath),
      multiSelectedPaths: remapPaths(state.multiSelectedPaths, moves),
      libraryActivePath: move(state.libraryActivePath),
      currentFolderPath: move(state.currentFolderPath),
      rootPaths: remapPaths(state.rootPaths, moves),
      ...(expandedChanged ? { expandedFolders: new Set(expanded) } : {}),
    };
  });
  useProcessStore.getState().setProcess((state) => ({
    thumbnails: remapPathKeys(state.thumbnails, moves),
    mediumThumbnails: remapPathKeys(state.mediumThumbnails, moves),
  }));
  // Pinned and root folders are kept in the settings.
  const { appSettings, handleSettingsChange } = useSettingsStore.getState();
  if (appSettings) {
    const pins = appSettings.pinnedFolders ?? [];
    const roots = appSettings.rootFolders ?? [];
    const movedPins = remapPaths(pins, moves);
    const movedRoots = remapPaths(roots, moves);
    if (movedPins !== pins || movedRoots !== roots) {
      const next = { ...appSettings } as AppSettings;
      if (movedPins !== pins) next.pinnedFolders = [...movedPins].sort((a, b) => a.localeCompare(b));
      if (movedRoots !== roots) next.rootFolders = movedRoots;
      void handleSettingsChange(next);
    }
  }
  const open = useEditorStore.getState().selectedImage?.path ?? null;
  const openPhoto = open ? movedPath(open, moves) : null;
  return { openPhoto: openPhoto !== open ? openPhoto : null };
}

/** After renaming `from[i]` to `to[i]` (see `followPathMoves`). */
export function followRenamedPaths(from: string[], to: string[]): { openPhoto: string | null } {
  return followPathMoves(from.flatMap((path, i) => (to[i] ? [{ from: path, to: to[i] }] : [])));
}

/** Reads the open session's picks and rejects again, after files were
 * renamed, moved, trashed or put back (also by undo) or relinked: the
 * catalog follows those, the path-keyed flags here do not. A flag set
 * while the list is read keeps its newer value. */
export async function reloadSessionFlags() {
  const directory = useLibraryStore.getState().sessionDirectory;
  if (!directory) return;
  const before = useLibraryStore.getState().imageFlags;
  let flags: Record<string, number> | null;
  try {
    flags = await invoke<Record<string, number>>('session_flags', { directory });
  } catch (e) {
    console.error('Picks and rejects could not be read again:', e);
    return;
  }
  if (!flags || useLibraryStore.getState().sessionDirectory !== directory) return;
  useLibraryStore.getState().setLibrary((state) => {
    const next = { ...flags };
    for (const [path, flag] of Object.entries(state.imageFlags)) {
      if (before[path] !== flag) next[path] = flag;
    }
    return { imageFlags: next };
  });
}
