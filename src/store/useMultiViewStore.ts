import { create } from 'zustand';
import { useEditorStore } from './useEditorStore';

const ENABLED_KEY = 'rawtonic.develop.multiView';

function storedEnabled(): boolean {
  try {
    return localStorage.getItem(ENABLED_KEY) !== 'off';
  } catch {
    return true;
  }
}

interface MultiViewState {
  /** Show several selected photographs side by side in Develop (N);
   * remembered. With one photograph selected Develop shows it alone. */
  enabled: boolean;
  /** Whether the multi view is on screen now (enabled, Develop, more than
   * one photograph selected and a native display). */
  active: boolean;
  /** Edits of the primary go to every selected photograph. Off by default
   * and never remembered (a forgotten switch would edit many photographs). */
  editAll: boolean;
  /** The native display cannot show it (no wgpu display, e.g. Linux). */
  unavailable: boolean;
  recipeRevision: number;
  setEnabled(enabled: boolean): void;
  toggle(): void;
  setActive(active: boolean): void;
  setEditAll(editAll: boolean): void;
  setUnavailable(unavailable: boolean): void;
  /** Recipes of shown photographs were written (Edit all selected): their
   * cells are developed again without waiting for new thumbnails. */
  recipesSaved(): void;
}

/**
 * Runs a zoom in the single view: the multi view shows every photograph
 * to fit (zoom is not shared between cells yet), so zooming the primary
 * first returns to the single view of it, as Lightroom's Survey goes to
 * Loupe. The zoom waits until the viewer has its single-view size.
 */
export function inSingleView(zoom: () => void, timeoutMs = 300) {
  if (!useMultiViewStore.getState().active) {
    zoom();
    return;
  }
  const before = useEditorStore.getState().baseRenderSize;
  let done = false;
  const run = () => {
    if (done) return;
    done = true;
    unsubscribe();
    clearTimeout(timer);
    zoom();
  };
  const unsubscribe = useEditorStore.subscribe((state) => {
    if (state.baseRenderSize !== before) run();
  });
  const timer = setTimeout(run, timeoutMs);
  useMultiViewStore.getState().setEnabled(false);
}

export const useMultiViewStore = create<MultiViewState>((set, get) => ({
  enabled: storedEnabled(),
  active: false,
  editAll: false,
  unavailable: false,
  recipeRevision: 0,
  setEnabled: (enabled) => {
    try {
      localStorage.setItem(ENABLED_KEY, enabled ? 'on' : 'off');
    } catch {
      // Not remembered; the switch still applies.
    }
    set({ enabled });
  },
  toggle: () => get().setEnabled(!get().enabled),
  setActive: (active) => set(active ? { active } : { active, editAll: false }),
  setEditAll: (editAll) => set({ editAll }),
  setUnavailable: (unavailable) => set({ unavailable }),
  recipesSaved: () => set((state) => ({ recipeRevision: state.recipeRevision + 1 })),
}));
