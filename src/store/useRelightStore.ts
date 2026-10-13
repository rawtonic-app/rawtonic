import { create } from "zustand";

/** Transient tool UI, never part of a photograph's recipe. `solo` previews
 * only that light (useImageProcessing `withViewStates`). */
export const useRelightStore = create<{
  panelOpen: boolean;
  showHandles: boolean;
  selected: string | null;
  solo: string | null;
}>(() => ({ panelOpen: false, showHandles: true, selected: null, solo: null }));
export const setRelightPanelOpen = (panelOpen: boolean) =>
  useRelightStore.setState(panelOpen ? { panelOpen } : { panelOpen, solo: null });
