import i18n from 'i18next';
import { useEffect } from 'react';
import { create } from 'zustand';
import { notify } from '../activity/notify';
import { Invokes } from '../components/ui/AppProperties';
import { useEditorStore } from '../store/useEditorStore';
import { useLibraryStore } from '../store/useLibraryStore';
import { useUIStore } from '../store/useUIStore';
import {
  canMoveInStack,
  canSplitStack,
  keepSelectionShown,
  refreshStacks,
  stackKey,
  useStackStore,
  type StackChange,
} from '../store/useStackStore';
import { registerAppCommands } from './appCommands';
import { stackAction } from './appUndo';
import { invokeInOrder } from './catalogWrites';

/** What a stack command did (`StackResult` in src-tauri/src/stacks.rs). */
interface StackResult {
  library: string;
  change: StackChange;
}

/** The Auto-Stack dialog (`AutoStackDialog`): open with the photos it looks at. */
export const useAutoStackDialog = create<{
  paths: string[] | null;
  open(paths: string[]): void;
  close(): void;
}>((set) => ({
  paths: null,
  open: (paths) => set({ paths: [...paths] }),
  close: () => set({ paths: null }),
}));

/** The selected photos, the active one first (Group into Stack makes it the
 * top). The active photo is the one the Navigator and the outline show: the
 * photo of the last plain click, which ⌘-click and ⇧-click leave in place (as
 * in Lightroom: click the best frame first, then add the rest). The photo the
 * last modifier click landed on (`libraryActivePath`) is the fallback. */
function selection(): string[] {
  const { multiSelectedPaths, libraryActivePath } = useLibraryStore.getState();
  const shown = useEditorStore.getState().selectedImage?.path;
  const paths = [...multiSelectedPaths];
  const active = [shown, libraryActivePath].find((path) => path && paths.includes(path));
  if (active) return [active, ...paths.filter((p) => p !== active)];
  return paths;
}

const inStack = (path: string) => useStackStore.getState().byPath.has(stackKey(path));

/** Keeps the selection to photos the grid shows (`keepSelectionShown`). */
export const selectShownMembers = () => keepSelectionShown(useStackStore.getState().byPath);

async function run(command: Invokes, args: Record<string, unknown>, paths: string[]) {
  if (!paths.length) return;
  try {
    const result = await invokeInOrder<StackResult>(command, args);
    stackAction(result.library, result.change, paths);
    await refreshStacks(result.library);
  } catch (error) {
    notify.error(i18n.t('stacks.failed', { error: String(error) }));
  }
}

export async function groupIntoStack(paths = selection()) {
  await run(Invokes.StacksGroup, { paths }, paths);
  // Albums list members singly, so nothing changes on screen there: say so.
  if (paths.length > 1 && useLibraryStore.getState().activeAlbumId !== null)
    notify.info(i18n.t('stacks.albumNote', { count: paths.length }), { autoClose: 5000 });
}
export const unstack = (paths = selection()) => run(Invokes.StacksUnstack, { paths }, paths);
export const removeFromStack = (paths = selection()) => run(Invokes.StacksRemove, { paths }, paths);
export const moveToTopOfStack = (path = selection()[0]) =>
  path ? run(Invokes.StacksMoveToTop, { path }, [path]) : Promise.resolve();
/** Split Stack: the active photo (`selection`, the first), else the first
 * selected member of an expanded stack below its top (e.g. a ⇧-click range;
 * the backend takes the first path that applies). */
export const splitStack = (paths = selection()) => run(Invokes.StacksSplit, { paths }, paths);
/** Move Up in Stack (`up`) or Move Down in Stack, the same way. */
export const moveInStack = (up: boolean, paths = selection()) =>
  run(Invokes.StacksMove, { paths, up }, paths);

/** Whether a selected photo can be split (`splittable`) or moved up/down
 * (`movable`) in a stack: the menu greys Split Stack, Move Up and Move Down
 * out otherwise. A ⇧-click range inside a stack holds members the Navigator
 * does not show (the active photo stays first in `selection`). */
export const splittable = () => selection().some(canSplitStack);
export const movable = (up: boolean) => selection().some((path) => canMoveInStack(path, up));
export const toggleStacks = (paths = selection()) =>
  run(Invokes.StacksSetCollapsed, { paths, collapsed: null }, paths);
/** Collapse All / Expand All: the stacks of the shown folder's photos. */
export function setAllStacks(collapsed: boolean) {
  const paths = useLibraryStore.getState().imageList.map((f) => f.path).filter(inStack);
  return run(Invokes.StacksSetCollapsed, { paths, collapsed }, paths);
}

/** What Auto-Stack looks at: the selection when it holds more than one
 * photo, else the whole shown folder (Lightroom acts on the view). */
export function autoStackScope(): string[] {
  const { multiSelectedPaths, imageList } = useLibraryStore.getState();
  return multiSelectedPaths.length > 1 ? [...multiSelectedPaths] : imageList.map((f) => f.path);
}

const libraryShown = () => useUIStore.getState().activeView === 'library';
const selected = () => useLibraryStore.getState().multiSelectedPaths;

/** Registers the stack commands (menu, keys, palette) and keeps the open
 * library's stacks read. */
export function useStackCommands() {
  useEffect(() => {
    void refreshStacks();
    const stopWatching = useLibraryStore.subscribe((state, before) => {
      if (state.sessionDirectory !== before.sessionDirectory || state.imageList !== before.imageList)
        void refreshStacks(state.sessionDirectory);
    });
    const unregister = registerAppCommands({
      stack_group: {
        enabled: () => libraryShown() && selected().length > 1,
        disabledHint: () => i18n.t('commandPalette.selectTwoPhotos'),
        run: () => void groupIntoStack(),
      },
      stack_unstack: {
        enabled: () => libraryShown() && selected().some(inStack),
        run: () => void unstack(),
      },
      stack_remove: {
        enabled: () => libraryShown() && selected().some(inStack),
        run: () => void removeFromStack(),
      },
      stack_move_top: {
        enabled: () => libraryShown() && selected().length === 1 && inStack(selected()[0]),
        run: () => void moveToTopOfStack(),
      },
      // Split Stack, Move Up/Down in Stack: ⇧[ and ⇧] act on the active photo
      // a ⇧-click range first (see `selection`).
      stack_split: {
        enabled: () => libraryShown() && splittable(),
        disabledHint: () => i18n.t('commandPalette.expandStackFirst'),
        run: () => void splitStack(),
      },
      stack_move_up: {
        enabled: () => libraryShown() && movable(true),
        disabledHint: () => i18n.t('commandPalette.selectStackMember'),
        run: () => void moveInStack(true),
      },
      stack_move_down: {
        enabled: () => libraryShown() && movable(false),
        disabledHint: () => i18n.t('commandPalette.selectStackMember'),
        run: () => void moveInStack(false),
      },
      stack_toggle: {
        enabled: () => libraryShown() && selected().some(inStack),
        run: () => void toggleStacks(),
      },
      stack_collapse_all: {
        enabled: () => libraryShown() && useStackStore.getState().stacks.length > 0,
        run: () => void setAllStacks(true),
      },
      stack_expand_all: {
        enabled: () => libraryShown() && useStackStore.getState().stacks.length > 0,
        run: () => void setAllStacks(false),
      },
      auto_stack: {
        enabled: () => libraryShown() && !!useLibraryStore.getState().sessionDirectory
          && useLibraryStore.getState().imageList.length > 1,
        run: () => useAutoStackDialog.getState().open(autoStackScope()),
      },
    });
    return () => {
      stopWatching();
      unregister();
    };
  }, []);
}
