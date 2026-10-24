import { cleanup, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { useKeyboardShortcuts } from "./useKeyboardShortcuts";
import { useLibraryStore } from "../store/useLibraryStore";
import { useSettingsStore } from "../store/useSettingsStore";
import { useUIStore } from "../store/useUIStore";
import { useEditorStore } from "../store/useEditorStore";
import type { AppSettings, ImageFile, SelectedImage } from "../components/ui/AppProperties";
import { readModuleLayout } from "../utils/panelLayout";
import { toggleWindowFullScreen } from "../utils/photoFullScreen";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(() => Promise.resolve()),
}));
vi.mock("react-toastify", () => ({
  toast: { error: vi.fn(), success: vi.fn() },
}));
vi.mock("../utils/photoFullScreen", () => ({
  toggleWindowFullScreen: vi.fn(() => Promise.resolve()),
}));

const files = ["/s/a.dng", "/s/b.dng", "/s/c.dng"].map((path) => ({ path }) as ImageFile);
const props = {
  sortedImageList: files,
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
