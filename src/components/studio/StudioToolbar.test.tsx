import "../../i18n";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import StudioWorkspace from "./StudioWorkspace";
import { useEditorStore } from "../../store/useEditorStore";
import { useUIStore } from "../../store/useUIStore";
import { useLibraryStore } from "../../store/useLibraryStore";
import { useSettingsStore } from "../../store/useSettingsStore";
import type { AppSettings, SelectedImage } from "../ui/AppProperties";

vi.mock("../panel/Editor", () => ({ default: () => null }));
vi.mock("./StudioDevelop", () => ({
  default: () => null,
  ToolGroup: () => null,
}));
vi.mock("./StudioNavigation", () => ({ default: () => null }));

beforeEach(() => {
  HTMLDialogElement.prototype.close = vi.fn();
  HTMLDialogElement.prototype.showModal = vi.fn();
  vi.stubGlobal(
    "ResizeObserver",
    class {
      observe() {}
      disconnect() {}
    },
  );
  useUIStore.setState({ activeView: "editor" });
  useEditorStore.setState({
    selectedImage: {
      path: "/qa/current.dng",
      width: 6000,
      height: 4000,
      isRaw: true,
      isReady: true,
    } as SelectedImage,
  });
  useLibraryStore.setState({ multiSelectedPaths: ["/qa/other.dng"] });
});
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

it("sends the open photo to Paste and Reset instead of the click event or old Library selection", () => {
  const paste = vi.fn(),
    reset = vi.fn(),
    copy = vi.fn(),
    noop = vi.fn();
  render(
    <StudioWorkspace
      files={[]}
      flagCounts={{ all: 0, picked: 0, rejected: 0, unflagged: 0 }}
      transformWrapperRef={{ current: null }}
      onSelect={noop}
      onFilmSelect={noop}
      onOpen={noop}
      onNavigate={noop}
      onBack={noop}
      onRate={noop}
      onContextMenu={noop}
      onEditorContextMenu={noop}
      onCopy={copy}
      onPaste={paste}
      onReset={reset}
      onZoom={noop}
      requestThumbnails={noop}
      settings={null}
      onSettings={noop}
    />,
  );
  fireEvent.click(screen.getByRole("button", { name: "Reset all adjustments" }));
  fireEvent.click(screen.getByRole("button", { name: "Paste adjustments" }));
  fireEvent.click(screen.getByRole("button", { name: "Copy adjustments" }));
  expect(reset).toHaveBeenCalledExactlyOnceWith(["/qa/current.dng"]);
  expect(paste).toHaveBeenCalledExactlyOnceWith(["/qa/current.dng"]);
  expect(copy).toHaveBeenCalledExactlyOnceWith();
  act(() => useEditorStore.setState((state) => ({ selectedImage: { ...state.selectedImage!, isReady: false } })));
  fireEvent.click(screen.getByRole("button", { name: "Reset all adjustments" }));
  fireEvent.click(screen.getByRole("button", { name: "Paste adjustments" }));
  fireEvent.click(screen.getByRole("button", { name: "Copy adjustments" }));
  expect(reset).toHaveBeenCalledTimes(1);
  expect(paste).toHaveBeenCalledTimes(1);
  expect(copy).toHaveBeenCalledTimes(1);
});

it("has one Before/After button: it toggles the split and names both keys", () => {
  useSettingsStore.setState({ appSettings: { keybinds: {} } as unknown as AppSettings, osPlatform: "macos" });
  useEditorStore.setState({ splitView: false, showOriginal: false });
  const noop = vi.fn();
  render(
    <StudioWorkspace
      files={[]}
      flagCounts={{ all: 0, picked: 0, rejected: 0, unflagged: 0 }}
      transformWrapperRef={{ current: null }}
      onSelect={noop}
      onFilmSelect={noop}
      onOpen={noop}
      onNavigate={noop}
      onBack={noop}
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
  expect(screen.queryByRole("button", { name: "Before" })).toBeNull();
  expect(screen.queryByRole("button", { name: "Split" })).toBeNull();
  const button = screen.getByRole("button", { name: "Before/After" });
  expect(button.title).toBe("Before/After (Y) · hold \\ for the whole photo before");
  fireEvent.click(button);
  expect(useEditorStore.getState().splitView).toBe(true);
  expect(button.getAttribute("aria-pressed")).toBe("true");
  fireEvent.click(button);
  expect(useEditorStore.getState().splitView).toBe(false);
});

it("shows the export turning and naming AI Denoise while a photo waits for it", async () => {
  const { useProcessStore } = await import("../../store/useProcessStore");
  const { Status } = await import("../ui/ExportImportProperties");
  useProcessStore.setState({
    exportState: { status: Status.Exporting, errorMessage: "", progress: { current: 0, total: 1 }, denoising: ["/qa/current.dng"] },
  });
  const noop = vi.fn();
  render(
    <StudioWorkspace
      files={[]}
      flagCounts={{ all: 0, picked: 0, rejected: 0, unflagged: 0 }}
      transformWrapperRef={{ current: null }}
      onSelect={noop}
      onFilmSelect={noop}
      onOpen={noop}
      onNavigate={noop}
      onBack={noop}
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
  const button = screen.getByRole("button", { name: "Exporting 0 / 1 · AI Denoise…" });
  expect(button.querySelector(".animate-spin")).not.toBeNull();
  act(() => useProcessStore.setState((s) => ({ exportState: { ...s.exportState, denoising: [], progress: { current: 1, total: 1 } } })));
  expect(screen.getByRole("button", { name: "Exporting 1 / 1" })).toBe(button);
  act(() => useProcessStore.setState({ exportState: { status: Status.Idle, errorMessage: "", progress: { current: 0, total: 0 } } }));
});
