import "../../i18n";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import StudioWorkspace from "./StudioWorkspace";
import { useEditorStore } from "../../store/useEditorStore";
import { useLibraryStore } from "../../store/useLibraryStore";
import { useUIStore } from "../../store/useUIStore";
import { useSettingsStore } from "../../store/useSettingsStore";
import { Panel, type AppSettings, type ImageFile, type SelectedImage } from "../ui/AppProperties";
import { IDLE_CURSOR_MS } from "./useIdleCursor";
import { followPhotoFullScreen } from "../../utils/photoFullScreen";
import { useQuickAdjust } from "./useQuickAdjust";

vi.mock("../../utils/photoFullScreen", () => ({ followPhotoFullScreen: vi.fn(() => Promise.resolve()) }));
const editorPointerDown = vi.fn();
const editorWheel = vi.fn();
const editorMenu = vi.fn();
vi.mock("./useQuickAdjust", () => ({ useQuickAdjust: vi.fn(() => null) }));
vi.mock("../panel/Editor", () => ({
  // As the real Editor: a native wheel listener on its container.
  default: () => (
    <div
      data-testid="native-viewer"
      ref={(el) => el?.addEventListener("wheel", editorWheel)}
      onPointerDownCapture={editorPointerDown}
      onContextMenu={editorMenu}
    />
  ),
}));
vi.mock("./StudioDevelop", () => ({
  default: () => <div data-testid="develop-tools" />,
  ToolGroup: () => null,
}));
vi.mock("./StudioNavigation", () => ({
  default: ({ develop }: { develop?: boolean }) => (
    <div data-testid="navigation" data-develop={String(Boolean(develop))} />
  ),
}));

const files = ["/qa/a.dng", "/qa/b.dng"].map(
  (path) =>
    ({
      path,
      rating: 0,
      is_raw: true,
      is_edited: false,
      tags: path.endsWith("a.dng") ? ["user:harbour", "user:boat"] : null,
      exif: null,
      modified: 0,
      is_virtual_copy: false,
      is_cloud_placeholder: false,
      group_id: null,
    }) as ImageFile,
);
const onBack = vi.fn();
const onSelect = vi.fn();
const onShow = vi.fn();

beforeEach(() => {
  vi.clearAllMocks();
  HTMLDialogElement.prototype.close = vi.fn();
  HTMLDialogElement.prototype.showModal = vi.fn();
  vi.stubGlobal(
    "ResizeObserver",
    class {
      observe() {}
      disconnect() {}
    },
  );
  useSettingsStore.setState({
    appSettings: { keybinds: {} } as unknown as AppSettings,
    osPlatform: "macos",
  });
  useLibraryStore.setState({ imageList: files, multiSelectedPaths: ["/qa/a.dng"], libraryActivePath: "/qa/a.dng" });
  useUIStore.setState((s) => ({
    activeView: "library",
    isFullScreen: false,
    libraryLoupe: false,
    infoOverlay: false,
    studioToolLayout: "tools-left",
    activePanel: Panel.Adjustments,
    uiVisibility: { ...s.uiVisibility, modules: undefined },
  }));
  useEditorStore.setState({
    selectedImage: {
      path: "/qa/a.dng",
      width: 6000,
      height: 4000,
      isRaw: true,
      isReady: true,
      exif: { Make: "Maker", Model: "Maker M1", LensModel: "35mm", DateTimeOriginal: "2026:10:20 10:00:00" },
    } as unknown as SelectedImage,
  });
});
afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

const noop = () => {};
const workspace = (list = files) =>
  render(
    <StudioWorkspace
      files={list}
      onShow={onShow}
      flagCounts={{ all: 2, picked: 0, rejected: 0, unflagged: 2 }}
      transformWrapperRef={{ current: null }}
      onSelect={onSelect}
      onFilmSelect={noop}
      onOpen={noop}
      onNavigate={noop}
      onBack={onBack}
      onRate={noop}
      onContextMenu={noop}
      onEditorContextMenu={noop}
      onCopy={noop}
      onPaste={noop}
      onReset={noop}
      onZoom={noop}
      requestThumbnails={noop}
      settings={null}
      onSettings={noop}
    />,
  );
const key = (k: string) => fireEvent.keyDown(document, { key: k, code: `Key${k.toUpperCase()}` });

it("opens the loupe on a double-click in the grid: the native viewer beside the Library tools", () => {
  const { container } = workspace();
  expect(screen.queryByTestId("native-viewer")).toBeNull();
  const tile = container.querySelector(".studio-photo-open")!;
  fireEvent.doubleClick(tile);
  expect(useUIStore.getState().libraryLoupe).toBe(true);
  expect(screen.getByTestId("native-viewer")).toBeTruthy();
  // The Library tools stay beside it (not Develop's), the grid is gone.
  expect(screen.getByTestId("navigation").dataset.develop).toBe("false");
  expect(screen.queryByTestId("develop-tools")).toBeNull();
  expect(container.querySelector(".studio-photo-open")).toBeNull();
  expect(container.querySelector(".studio-loupe-bar")).not.toBeNull();
  expect(container.querySelector(".studio-workspace.module-library.is-library-loupe")).not.toBeNull();
});

it("goes from the loupe back to the grid with G and its button, and opens it again with E", () => {
  useUIStore.setState({ libraryLoupe: true });
  const { container } = workspace();
  key("g");
  expect(useUIStore.getState().libraryLoupe).toBe(false);
  expect(onBack).toHaveBeenCalledOnce();
  expect(container.querySelector(".studio-loupe-bar")).toBeNull();
  key("e");
  expect(useUIStore.getState()).toMatchObject({ libraryLoupe: true, activeView: "library" });
  fireEvent.click(screen.getByTitle("Back to the grid (G)"));
  expect(useUIStore.getState().libraryLoupe).toBe(false);
});

it("opens Develop from the loupe with D, and E stays the eraser key there", () => {
  useUIStore.setState({ libraryLoupe: true });
  workspace();
  key("d");
  expect(useUIStore.getState().activeView).toBe("editor");
  key("e");
  expect(useUIStore.getState().activeView).toBe("editor");
});

it("leaves Develop's crop and mask states out of the loupe", () => {
  useUIStore.setState({ activePanel: Panel.Crop });
  useEditorStore.setState({ activeMaskContainerId: "layer-1" });
  workspace();
  act(() => useUIStore.getState().setUI({ libraryLoupe: true }));
  expect(useUIStore.getState().activePanel).toBe(Panel.Adjustments);
  expect(useEditorStore.getState().activeMaskContainerId).toBeNull();
});

it("shows only the photograph in the full screen from the Library, without bars, panels or strips", () => {
  useUIStore.getState().togglePanels("all");
  const { container } = workspace();
  act(() => useUIStore.getState().toggleFullScreen());
  expect(screen.getByTestId("native-viewer")).toBeTruthy();
  expect(container.querySelector(".studio-workspace.is-image-fullscreen")).not.toBeNull();
  // Everything else is hidden by the full screen's style (studio.css), the
  // grid's bars are not there at all.
  expect(container.querySelector(".studio-library-toolbar")).toBeNull();
  expect(container.querySelector(".studio-library-footer")).toBeNull();
  act(() => useUIStore.getState().toggleFullScreen());
  expect(screen.queryByTestId("native-viewer")).toBeNull();
});

it("hides the pointer over the full screen after it rests, and shows it on a move", () => {
  vi.useFakeTimers();
  useUIStore.setState({ activeView: "editor" });
  const { container } = workspace();
  act(() => useUIStore.getState().toggleFullScreen());
  expect(container.querySelector(".is-cursor-idle")).toBeNull();
  act(() => vi.advanceTimersByTime(IDLE_CURSOR_MS + 10));
  expect(container.querySelector(".is-cursor-idle")).not.toBeNull();
  act(() => {
    window.dispatchEvent(new Event("pointermove"));
  });
  expect(container.querySelector(".is-cursor-idle")).toBeNull();
});

it("shows the photograph's facts and keywords read only over Develop's viewer", () => {
  useUIStore.setState({ activeView: "editor", infoOverlay: true });
  workspace();
  const info = screen.getByRole("complementary", { name: "Photograph info" });
  expect(info.textContent).toContain("a.dng");
  expect(info.textContent).toContain("Maker M1 · 35mm");
  expect(info.textContent).toContain("Keywords: boat, harbour");
  expect(info.querySelector("input, textarea, button")).toBeNull();
});

it("leaves the full screen and the loupe when the photograph goes away", () => {
  useUIStore.setState({ libraryLoupe: true, isFullScreen: true });
  workspace();
  act(() => useEditorStore.setState({ selectedImage: null }));
  expect(useUIStore.getState()).toMatchObject({ libraryLoupe: false, isFullScreen: false });
});

it("takes the whole screen for the photograph alone and gives it back on leaving", () => {
  workspace();
  expect(followPhotoFullScreen).not.toHaveBeenCalled();
  act(() => useUIStore.getState().toggleFullScreen());
  expect(followPhotoFullScreen).toHaveBeenLastCalledWith(true);
  act(() => useUIStore.getState().toggleFullScreen());
  expect(followPhotoFullScreen).toHaveBeenLastCalledWith(false);
});

it("puts Develop's crop and layer aside in the full screen and brings them back after it", () => {
  useUIStore.setState({ activeView: "editor", activePanel: Panel.Crop });
  useEditorStore.setState({ activeMaskContainerId: "layer-1", isStraightenActive: true });
  workspace();
  act(() => useUIStore.getState().toggleFullScreen());
  expect(useUIStore.getState().activePanel).toBe(Panel.Adjustments);
  expect(useEditorStore.getState()).toMatchObject({ activeMaskContainerId: null, isStraightenActive: false });
  act(() => useUIStore.getState().toggleFullScreen());
  expect(useUIStore.getState().activePanel).toBe(Panel.Crop);
  expect(useEditorStore.getState().activeMaskContainerId).toBe("layer-1");
});

it("does not switch cursor tools while the photograph is shown alone", () => {
  useUIStore.setState({ activeView: "editor" });
  workspace();
  act(() => useUIStore.getState().toggleFullScreen());
  fireEvent.keyDown(document, { key: "c", code: "KeyC" });
  expect(useUIStore.getState().activePanel).toBe(Panel.Adjustments);
});

it("shows a virtual copy with its source's flag in the info overlay", () => {
  useUIStore.setState({ activeView: "editor", infoOverlay: true });
  useLibraryStore.setState({ imageFlags: { "/qa/a.dng": 1 } });
  useEditorStore.setState((s) => ({
    selectedImage: { ...s.selectedImage!, path: "/qa/a.dng?vc=copy1" },
  }));
  workspace();
  const info = screen.getByRole("complementary", { name: "Photograph info" });
  expect(info.querySelector('[aria-label="Picked"]')).not.toBeNull();
});

it("puts the spot tool aside in the full screen and brings it back after it", () => {
  useUIStore.setState({ activeView: "editor" });
  useEditorStore.setState({ isSpotToolActive: true, showFaceBlemishes: true });
  workspace();
  act(() => useUIStore.getState().toggleFullScreen());
  expect(useEditorStore.getState()).toMatchObject({ isSpotToolActive: false, showFaceBlemishes: false });
  act(() => useUIStore.getState().toggleFullScreen());
  expect(useEditorStore.getState()).toMatchObject({ isSpotToolActive: true, showFaceBlemishes: true });
  useEditorStore.setState({ isSpotToolActive: false, showFaceBlemishes: false });
});

it("keeps Shift-drag and Cmd/Ctrl clicks (straighten, crop reset) from the viewer while it only shows", () => {
  useUIStore.setState({ libraryLoupe: true });
  workspace();
  const viewer = screen.getByTestId("native-viewer");
  fireEvent.pointerDown(viewer, { shiftKey: true });
  fireEvent.pointerDown(viewer, { metaKey: true });
  fireEvent.pointerDown(viewer, { ctrlKey: true });
  expect(editorPointerDown).not.toHaveBeenCalled();
  // A plain press still pans and zooms.
  fireEvent.pointerDown(viewer);
  expect(editorPointerDown).toHaveBeenCalledOnce();
  // Develop keeps its gestures.
  cleanup();
  editorPointerDown.mockClear();
  useUIStore.setState({ activeView: "editor", libraryLoupe: false });
  workspace();
  fireEvent.pointerDown(screen.getByTestId("native-viewer"), { shiftKey: true });
  expect(editorPointerDown).toHaveBeenCalledOnce();
});

it("shows the next photograph when the shown one leaves the filtered list, and the grid when none is left", () => {
  useUIStore.setState({ libraryLoupe: true });
  const { rerender } = workspace();
  const props = (list: ImageFile[]) => ({
    files: list,
    onShow,
    flagCounts: { all: 0, picked: 0, rejected: 0, unflagged: 0 },
    transformWrapperRef: { current: null },
    onSelect,
    onFilmSelect: noop,
    onOpen: noop,
    onNavigate: noop,
    onBack,
    onRate: noop,
    onContextMenu: noop,
    onEditorContextMenu: noop,
    onCopy: noop,
    onPaste: noop,
    onReset: noop,
    onZoom: noop,
    requestThumbnails: noop,
    settings: null,
    onSettings: noop,
  });
  rerender(<StudioWorkspace {...props([files[1]])} />);
  expect(onShow).toHaveBeenCalledWith("/qa/b.dng");
  expect(useUIStore.getState().libraryLoupe).toBe(true);
  rerender(<StudioWorkspace {...props([])} />);
  expect(useUIStore.getState().libraryLoupe).toBe(false);
});

it("turns Quick Adjust off while Develop's photograph is shown alone", () => {
  useUIStore.setState({ activeView: "editor" });
  workspace();
  expect(vi.mocked(useQuickAdjust)).toHaveBeenLastCalledWith(true);
  act(() => useUIStore.getState().toggleFullScreen());
  expect(vi.mocked(useQuickAdjust)).toHaveBeenLastCalledWith(false);
});

it("opens the loupe with E on the Library's active photograph, loading it when a modified click only moved the active one", () => {
  useLibraryStore.setState({ libraryActivePath: "/qa/b.dng", multiSelectedPaths: ["/qa/a.dng", "/qa/b.dng"] });
  workspace();
  key("e");
  expect(onShow).toHaveBeenCalledWith("/qa/b.dng");
  expect(useUIStore.getState().libraryLoupe).toBe(true);
});

it("keeps a wheel with Cmd/Ctrl held (the editor's crop zoom) from the photograph while it only shows; a pinch still zooms", () => {
  useUIStore.setState({ libraryLoupe: true });
  workspace();
  const viewer = screen.getByTestId("native-viewer");
  fireEvent.keyDown(window, { key: "Meta" });
  fireEvent.wheel(viewer, { deltaY: -10, metaKey: true });
  expect(editorWheel).not.toHaveBeenCalled();
  fireEvent.keyUp(window, { key: "Meta" });
  // A trackpad pinch: ctrlKey set by the system, no key held.
  fireEvent.wheel(viewer, { deltaY: -10, ctrlKey: true });
  expect(editorWheel).toHaveBeenCalledOnce();
});

it("opens no context menu of edit commands over the photograph shown alone or in the loupe", () => {
  useUIStore.setState({ activeView: "editor" });
  workspace();
  act(() => useUIStore.getState().toggleFullScreen());
  fireEvent.contextMenu(screen.getByTestId("native-viewer"));
  expect(editorMenu).not.toHaveBeenCalled();
  act(() => useUIStore.getState().toggleFullScreen());
  fireEvent.contextMenu(screen.getByTestId("native-viewer"));
  expect(editorMenu).toHaveBeenCalledOnce();
});
