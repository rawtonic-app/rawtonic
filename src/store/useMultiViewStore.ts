import { create } from 'zustand';

const ENABLED_KEY = 'rawtonic.develop.multiView';
const ZOOM_LOCK_KEY = 'rawtonic.develop.multiViewZoomLock';

function stored(key: string): boolean {
  try {
    return localStorage.getItem(key) !== 'off';
  } catch {
    return true;
  }
}

function remember(key: string, on: boolean) {
  try {
    localStorage.setItem(key, on ? 'on' : 'off');
  } catch {
    // Not remembered; the switch still applies.
  }
}

interface MultiViewState {
  /** Show several selected photographs side by side in Develop (N);
   * remembered. With one photograph selected Develop shows it alone. */
  enabled: boolean;
  /** Whether the multi view is on screen now (enabled, Develop, more than
   * one photograph shown and a native display). */
  active: boolean;
  /** Edits of the primary go to every selected photograph. Off by default
   * and never remembered (a forgotten switch would edit many photographs). */
  editAll: boolean;
  /** The native display cannot show it (no wgpu display, e.g. Linux). */
  unavailable: boolean;
  recipeRevision: number;
  /** The pinned reference photograph (Capture One's compare variant,
   * ⇧Return): shown first with an orange frame, not part of the selection,
   * kept while the primary steps through the rest; ⌘Return clears it. */
  reference: string | null;
  /** Zooming the primary zooms and pans every cell to the same point
   * (Lightroom's zoom lock); on by default, remembered. Off, only the
   * primary zooms (Capture One). */
  zoomLock: boolean;
  setEnabled(enabled: boolean): void;
  toggle(): void;
  setActive(active: boolean): void;
  setEditAll(editAll: boolean): void;
  setUnavailable(unavailable: boolean): void;
  /** Recipes of shown photographs were written (Edit all selected): their
   * cells are developed again without waiting for new thumbnails. */
  recipesSaved(): void;
  setReference(reference: string | null): void;
  setZoomLock(zoomLock: boolean): void;
}

export const useMultiViewStore = create<MultiViewState>((set, get) => ({
  enabled: stored(ENABLED_KEY),
  active: false,
  editAll: false,
  unavailable: false,
  recipeRevision: 0,
  reference: null,
  zoomLock: stored(ZOOM_LOCK_KEY),
  setEnabled: (enabled) => {
    remember(ENABLED_KEY, enabled);
    set({ enabled });
  },
  toggle: () => get().setEnabled(!get().enabled),
  setActive: (active) => set(active ? { active } : { active, editAll: false }),
  setEditAll: (editAll) => set({ editAll }),
  setUnavailable: (unavailable) => set({ unavailable }),
  recipesSaved: () => set((state) => ({ recipeRevision: state.recipeRevision + 1 })),
  setReference: (reference) => set({ reference }),
  setZoomLock: (zoomLock) => {
    remember(ZOOM_LOCK_KEY, zoomLock);
    set({ zoomLock });
  },
}));
