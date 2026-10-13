import { create } from "zustand";

/** Transient tool UI, never part of a photograph's recipe. */
export const useRelightStore = create<{
  panelOpen: boolean;
  showHandles: boolean;
  selected: string | null;
}>(() => ({ panelOpen: false, showHandles: true, selected: null }));
export const setRelightPanelOpen = (panelOpen: boolean) =>
  useRelightStore.setState({ panelOpen });
