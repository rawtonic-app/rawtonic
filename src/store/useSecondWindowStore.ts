import { create } from 'zustand';

/** What the second window shows (second_window.rs `SecondViewState`). */
export interface SecondWindowState {
  available: boolean;
  /** The page draws the photograph from JPEG frames (Linux, where WebKit
   * shows the main window's previews the same way); else the native
   * display draws it under the page. */
  pageDraws: boolean;
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

/** One frame the page draws (second_window/browser.rs `FrameLayer`). */
export interface SecondWindowFrame {
  id: string;
  width: number;
  height: number;
  /** The photograph's shown size, full-resolution px. */
  shown: [number, number];
  /** A region's part of the photograph (x, y, w, h in those px). */
  region: [number, number, number, number] | null;
}

/** What the page draws (Linux): the whole photograph and, zoomed in, the
 * sharp region over it. */
export interface SecondWindowFrames {
  path: string | null;
  whole: SecondWindowFrame | null;
  region: SecondWindowFrame | null;
}

export const SECOND_WINDOW_FRAMES_EVENT = 'second-window-frames';

export const closedSecondWindow: SecondWindowState = {
  available: true,
  pageDraws: false,
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
