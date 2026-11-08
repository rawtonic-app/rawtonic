import { create } from 'zustand';

/** What the second window shows (second_window.rs `SecondViewState`). */
export interface SecondWindowState {
  available: boolean;
  open: boolean;
  locked: boolean;
  path: string | null;
  /** The photograph's shown size (cropped, turned), full-resolution px. */
  shown: [number, number] | null;
  /** null: to fit; else device px per full-resolution px (1 = 100 %) and
   * the photograph point (0–1) at the window's centre. */
  zoom: { scale: number; center: [number, number] } | null;
}

export const SECOND_WINDOW_EVENT = 'second-window-state';

export const closedSecondWindow: SecondWindowState = {
  available: true,
  open: false,
  locked: false,
  path: null,
  shown: null,
  zoom: null
};

interface Store {
  state: SecondWindowState;
  set(state: SecondWindowState): void;
}

export const useSecondWindowStore = create<Store>((set) => ({
  state: closedSecondWindow,
  set: (state) => set({ state })
}));
