import i18n from 'i18next';
import { useEffect } from 'react';
import { create } from 'zustand';
import { notify } from '../activity/notify';
import { Invokes } from '../components/ui/AppProperties';
import { useLibraryStore } from '../store/useLibraryStore';
import { useUIStore } from '../store/useUIStore';
import { keepSelectionShown, refreshStacks, stackKey, useStackStore, type StackChange } from '../store/useStackStore';
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

/** The selected photos, the active one first (Group into Stack makes it the top). */
function selection(): string[] {
  const { multiSelectedPaths, libraryActivePath } = useLibraryStore.getState();
  const paths = [...multiSelectedPaths];
  if (libraryActivePath && paths.includes(libraryActivePath))
    return [libraryActivePath, ...paths.filter((p) => p !== libraryActivePath)];
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

export const groupIntoStack = (paths = selection()) => run(Invokes.StacksGroup, { paths }, paths);
export const unstack = (paths = selection()) => run(Invokes.StacksUnstack, { paths }, paths);
export const removeFromStack = (paths = selection()) => run(Invokes.StacksRemove, { paths }, paths);
export const moveToTopOfStack = (path = selection()[0]) =>
  path ? run(Invokes.StacksMoveToTop, { path }, [path]) : Promise.resolve();
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
