import { cleanup, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { useKeyboardShortcuts } from "./useKeyboardShortcuts";
import { useLibraryStore } from "../store/useLibraryStore";
import { useSettingsStore } from "../store/useSettingsStore";
import { useUIStore } from "../store/useUIStore";
import { useEditorStore } from "../store/useEditorStore";
import { Panel, type AppSettings, type ImageFile, type SelectedImage } from "../components/ui/AppProperties";
import { readModuleLayout } from "../utils/panelLayout";
import { toggleWindowFullScreen } from "../utils/photoFullScreen";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(() => Promise.resolve()),
}));
vi.mock("../activity/notify", () => ({
  notify: { error: vi.fn(), success: vi.fn(), info: vi.fn() },
}));
vi.mock("../utils/photoFullScreen", () => ({
  toggleWindowFullScreen: vi.fn(() => Promise.resolve()),
}));

const files = ["/s/a.dng", "/s/b.dng", "/s/c.dng"].map((path) => ({ path }) as ImageFile);
const props = {
  sortedImageList: files,
  handleUndoFileOperation: vi.fn(),
  handleBackToLibrary: vi.fn(),
  handleDeleteSelected: vi.fn(),
  handleImageSelect: vi.fn(),
  handlePasteFiles: vi.fn(),
  handleZoomChange: vi.fn(),
};
const press = (code: string, init: KeyboardEventInit = {}) =>
  window.dispatchEvent(
    new KeyboardEvent("keydown", {
      code,
      key: code.startsWith("Key") ? code.slice(3).toLowerCase() : code === "Space" ? " " : code,
      bubbles: true,
      cancelable: true,
      ...init,
    }),
  );
const open = (path: string) =>
  useEditorStore.setState({ selectedImage: { path, isReady: true } as SelectedImage });

beforeEach(() => {
  vi.clearAllMocks();
  useSettingsStore.setState({
    appSettings: { keybinds: {} } as unknown as AppSettings,
    osPlatform: "macos",
  });
  useLibraryStore.setState({
    imageList: files,
    multiSelectedPaths: ["/s/b.dng"],
    libraryActivePath: "/s/b.dng",
  });
  useUIStore.setState((s) => ({
    activeView: "library",
    isSettingsOpen: false,
    isFullScreen: false,
    libraryLoupe: false,
    infoOverlay: false,
    gridColumns: 3,
    uiVisibility: { ...s.uiVisibility, modules: undefined },
  }));
  open("/s/b.dng");
  renderHook(() => useKeyboardShortcuts(props));
});
afterEach(() => {
  cleanup();
  useEditorStore.setState({ selectedImage: null });
});

it("shows the photograph alone with F in Develop and leaves with F or Escape, staying in Develop", () => {
  useUIStore.setState({ activeView: "editor" });
  press("KeyF");
  expect(useUIStore.getState().isFullScreen).toBe(true);
  press("KeyF");
  expect(useUIStore.getState().isFullScreen).toBe(false);
  press("KeyF");
  press("Escape");
  expect(useUIStore.getState().isFullScreen).toBe(false);
  expect(useUIStore.getState().activeView).toBe("editor");
  expect(props.handleBackToLibrary).not.toHaveBeenCalled();
  expect(toggleWindowFullScreen).not.toHaveBeenCalled();
});

it("shows the Library's photograph alone with F and leaves it with Escape back to the grid", () => {
  press("KeyF");
  expect(useUIStore.getState().isFullScreen).toBe(true);
  press("Escape");
  expect(useUIStore.getState()).toMatchObject({ isFullScreen: false, libraryLoupe: false, activeView: "library" });
});

it("ignores F's key repeat, so holding F does not flicker in and out", () => {
  press("KeyF");
  press("KeyF", { repeat: true });
  expect(useUIStore.getState().isFullScreen).toBe(true);
});

it("keeps the window's full screen on F11", () => {
  press("F11");
  expect(toggleWindowFullScreen).toHaveBeenCalledOnce();
  expect(useUIStore.getState().isFullScreen).toBe(false);
});

it("opens the loupe with Space in the grid and goes back to the grid with Escape", () => {
  useEditorStore.setState({ selectedImage: null });
  press("Space");
  expect(useUIStore.getState().libraryLoupe).toBe(true);
  // The active photograph is opened for the viewer when it was not yet.
  expect(props.handleImageSelect).toHaveBeenCalledWith("/s/b.dng", false);
  press("Escape");
  expect(useUIStore.getState().libraryLoupe).toBe(false);
});

it("steps one photograph with the arrows in the loupe, wrapping, and zooms with Up/Down", () => {
  useUIStore.setState({ libraryLoupe: true });
  press("ArrowRight");
  expect(props.handleImageSelect).toHaveBeenLastCalledWith("/s/c.dng", false);
  expect(useLibraryStore.getState()).toMatchObject({ libraryActivePath: "/s/c.dng", multiSelectedPaths: ["/s/c.dng"] });
  open("/s/c.dng");
  press("ArrowRight");
  expect(props.handleImageSelect).toHaveBeenLastCalledWith("/s/a.dng", false);
  // Up/Down zoom as in Develop instead of moving a grid row.
  props.handleImageSelect.mockClear();
  press("ArrowDown");
  expect(props.handleImageSelect).not.toHaveBeenCalled();
  expect(props.handleZoomChange).toHaveBeenCalled();
  expect(useUIStore.getState().activeView).toBe("library");
});

it("steps with the arrows in the full screen opened from the Library", () => {
  useUIStore.setState({ isFullScreen: true });
  press("ArrowLeft");
  expect(props.handleImageSelect).toHaveBeenLastCalledWith("/s/a.dng", false);
  expect(useUIStore.getState().isFullScreen).toBe(true);
});

it("keeps the grid's arrows on rows while no loupe is shown", () => {
  press("ArrowUp");
  // b is in the first row of three: Up stays.
  expect(props.handleImageSelect).not.toHaveBeenCalled();
  press("ArrowRight");
  expect(props.handleImageSelect).toHaveBeenLastCalledWith("/s/c.dng", false);
});

it("leaves Tab and Shift+Tab without effect in the full screen, and keeps them outside it", () => {
  useUIStore.setState({ activeView: "editor", isFullScreen: true });
  const before = useUIStore.getState().uiVisibility.modules;
  press("Tab");
  press("Tab", { shiftKey: true });
  expect(useUIStore.getState().uiVisibility.modules).toBe(before);
  useUIStore.setState({ isFullScreen: false });
  press("Tab");
  expect(readModuleLayout(useUIStore.getState().uiVisibility.modules, "develop")).toMatchObject({ left: false });
});

it("shows the photograph's facts with I in Develop and the full screen, not in the Library grid", () => {
  useUIStore.setState({ activeView: "editor" });
  press("KeyI");
  expect(useUIStore.getState().infoOverlay).toBe(true);
  press("KeyI");
  expect(useUIStore.getState().infoOverlay).toBe(false);
  useUIStore.setState({ activeView: "library" });
  press("KeyI");
  expect(useUIStore.getState().infoOverlay).toBe(false);
  useUIStore.setState({ isFullScreen: true });
  press("KeyI");
  expect(useUIStore.getState().infoOverlay).toBe(true);
});

it("rates the shown photograph in the full screen", () => {
  useUIStore.setState({ isFullScreen: true });
  press("Digit4", { key: "4" });
  expect(useLibraryStore.getState().imageRatings["/s/b.dng"]).toBe(4);
});

it("leaves the full screen first on Escape, even with a picker or the crop open", () => {
  useUIStore.setState({ activeView: "editor", isFullScreen: true, activePanel: Panel.Crop });
  useEditorStore.setState({ isStraightenActive: true });
  press("Escape");
  expect(useUIStore.getState()).toMatchObject({ isFullScreen: false, activePanel: Panel.Crop });
  expect(useEditorStore.getState().isStraightenActive).toBe(true);
  useEditorStore.setState({ isStraightenActive: false });
  useUIStore.setState({ activePanel: Panel.Adjustments });
});

it("keeps the crop key from opening the crop tool in the full screen", () => {
  useUIStore.setState({ activeView: "editor", isFullScreen: true, activePanel: Panel.Adjustments });
  press("KeyS");
  expect(useUIStore.getState().activePanel).toBe(Panel.Adjustments);
});

it("rates and labels only the photograph shown in the full screen and the loupe, not the rest of the selection", () => {
  useLibraryStore.setState({ multiSelectedPaths: ["/s/a.dng", "/s/b.dng", "/s/c.dng"], imageRatings: {} });
  useUIStore.setState({ activeView: "editor", isFullScreen: true });
  press("Digit2", { key: "2" });
  expect(useLibraryStore.getState().imageRatings).toEqual({ "/s/b.dng": 2 });
  useUIStore.setState({ activeView: "library", isFullScreen: false, libraryLoupe: true });
  press("Digit5", { key: "5" });
  expect(useLibraryStore.getState().imageRatings).toEqual({ "/s/b.dng": 5 });
});

it("keeps the crop Escape restores across a full screen in the middle of cropping", () => {
  const before = { x: 0, y: 0, width: 100, height: 100, unit: "%" };
  const edited = { x: 10, y: 10, width: 50, height: 50, unit: "%" };
  useUIStore.setState({ activeView: "editor", activePanel: Panel.Adjustments });
  useEditorStore.setState((s) => ({ adjustments: { ...s.adjustments, crop: before } as never }));
  useUIStore.getState().setPanel(Panel.Crop);
  useEditorStore.setState((s) => ({ adjustments: { ...s.adjustments, crop: edited } as never }));
  // F: the crop tool is put aside and comes back after (StudioWorkspace).
  useUIStore.setState({ isFullScreen: true });
  useUIStore.getState().setPanel(Panel.Adjustments);
  useUIStore.setState({ isFullScreen: false });
  useUIStore.getState().setPanel(Panel.Crop);
  press("Escape");
  expect(useEditorStore.getState().adjustments.crop).toEqual(before);
  expect(useUIStore.getState().activePanel).toBe(Panel.Adjustments);
});

it("ignores recipe-changing keys in the full screen and keeps them outside it", () => {
  useUIStore.setState({ activeView: "editor", isFullScreen: true });
  useEditorStore.setState((s) => ({ adjustments: { ...s.adjustments, orientationSteps: 0 } as never }));
  press("BracketRight", { key: "]" });
  expect(useEditorStore.getState().adjustments.orientationSteps).toBe(0);
  useUIStore.setState({ isFullScreen: false });
  press("BracketRight", { key: "]" });
  expect(useEditorStore.getState().adjustments.orientationSteps).toBe(1);
});

it("loads the Library's active photograph before F shows it alone", () => {
  useLibraryStore.setState({ libraryActivePath: "/s/c.dng", multiSelectedPaths: ["/s/b.dng", "/s/c.dng"] });
  press("KeyF");
  expect(props.handleImageSelect).toHaveBeenCalledWith("/s/c.dng", false);
  expect(useUIStore.getState().isFullScreen).toBe(true);
});

it("ignores recipe keys in the Library's loupe, but keeps undoing file operations there", () => {
  useUIStore.setState({ activeView: "library", libraryLoupe: true });
  useEditorStore.setState((s) => ({ adjustments: { ...s.adjustments, orientationSteps: 0 } as never }));
  press("BracketRight", { key: "]" });
  expect(useEditorStore.getState().adjustments.orientationSteps).toBe(0);
  press("KeyZ", { metaKey: true });
  expect(props.handleUndoFileOperation).toHaveBeenCalledOnce();
});

it("ignores F in the middle of a stroke or drag, also with a second pointer released meanwhile", () => {
  useUIStore.setState({ activeView: "editor", activePanel: Panel.Masks });
  useEditorStore.setState({ activeMaskContainerId: "layer-1", activeMaskId: "brush-1" });
  const pointer = (type: string, pointerId: number) =>
    window.dispatchEvent(new PointerEvent(type, { bubbles: true, pointerId }));
  try {
    // The stroke keeps its points in screen coordinates until its release:
    // the viewer must not change its size before that.
    pointer("pointerdown", 1);
    press("KeyF");
    expect(useUIStore.getState().isFullScreen).toBe(false);
    expect(useEditorStore.getState()).toMatchObject({ activeMaskContainerId: "layer-1", activeMaskId: "brush-1" });
    // A second touch comes and goes: the first stroke is still held.
    pointer("pointerdown", 2);
    pointer("pointerup", 2);
    press("KeyF");
    expect(useUIStore.getState().isFullScreen).toBe(false);
    // Released: nothing was waiting; the next F opens it.
    pointer("pointerup", 1);
    expect(useUIStore.getState().isFullScreen).toBe(false);
    press("KeyF");
    expect(useUIStore.getState().isFullScreen).toBe(true);
    press("KeyF");
    // A release missed (focus lost mid-stroke) keeps F ignored only until then.
    pointer("pointerdown", 3);
    window.dispatchEvent(new Event("blur"));
    press("KeyF");
    expect(useUIStore.getState().isFullScreen).toBe(true);
  } finally {
    useEditorStore.setState({ activeMaskContainerId: null, activeMaskId: null });
    useUIStore.setState({ isFullScreen: false });
  }
});
