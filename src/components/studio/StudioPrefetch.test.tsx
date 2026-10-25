import "../../i18n";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { cleanup, render } from "@testing-library/react";
import StudioWorkspace from "./StudioWorkspace";
import { useEditorStore } from "../../store/useEditorStore";
import { useLibraryStore } from "../../store/useLibraryStore";
import { useUIStore } from "../../store/useUIStore";
import { useSettingsStore } from "../../store/useSettingsStore";
import { Panel, type AppSettings, type ImageFile, type SelectedImage } from "../ui/AppProperties";
import { ContextMenuProvider } from "../../context/ContextMenuContext";

const invoke = vi.hoisted(() => vi.fn(() => Promise.resolve(undefined)));
vi.mock("@tauri-apps/api/core", () => ({ invoke, convertFileSrc: (path: string) => path }));
vi.mock("../../utils/photoFullScreen", () => ({ followPhotoFullScreen: vi.fn(() => Promise.resolve()) }));
vi.mock("./useQuickAdjust", () => ({ useQuickAdjust: vi.fn(() => null) }));
vi.mock("../panel/Editor", () => ({ default: () => <div data-testid="native-viewer" /> }));
vi.mock("./StudioDevelop", () => ({ default: () => null, ToolGroup: () => null }));
vi.mock("./StudioNavigation", () => ({ default: () => null }));

const files = ["/qa/a.dng", "/qa/b.dng", "/qa/c.dng", "/qa/d.dng"].map(
  (path) =>
    ({
      path,
      rating: 0,
      is_raw: true,
      is_edited: false,
      tags: null,
      exif: null,
      modified: 0,
      is_virtual_copy: false,
      is_cloud_placeholder: false,
      group_id: null,
    }) as ImageFile,
);

beforeEach(() => {
  invoke.mockClear();
  HTMLDialogElement.prototype.close = vi.fn();
  HTMLDialogElement.prototype.showModal = vi.fn();
  vi.stubGlobal(
    "ResizeObserver",
    class {
      observe() {}
      disconnect() {}
    },
  );
  useSettingsStore.setState({ appSettings: { keybinds: {} } as unknown as AppSettings, osPlatform: "macos" });
  useLibraryStore.setState({ imageList: files, multiSelectedPaths: ["/qa/b.dng"], libraryActivePath: "/qa/b.dng" });
  useEditorStore.setState({
    selectedImage: { path: "/qa/b.dng", width: 6000, height: 4000, isRaw: true, isReady: true, isDecoded: true } as unknown as SelectedImage,
  });
});
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

const noop = () => {};
const workspace = () =>
  render(
    <StudioWorkspace
      files={files}
      onShow={noop}
      flagCounts={{ all: 4, picked: 0, rejected: 0, unflagged: 4 }}
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
    { wrapper: ({ children }) => <ContextMenuProvider>{children}</ContextMenuProvider> },
  );
const prepared = () =>
  (invoke.mock.calls as unknown[][]).filter(([command]) => command === "prepare_preview_proxies").map(([, args]) => args);

it("prepares the decoded photograph's neighbours in the Library's loupe as in Develop", () => {
  useUIStore.setState({ activeView: "library", libraryLoupe: true, isFullScreen: false, activePanel: Panel.Adjustments });
  workspace();
  expect(prepared()).toEqual([{ paths: ["/qa/c.dng", "/qa/d.dng", "/qa/a.dng"] }]);
});

it("prepares nothing for the Library's grid", () => {
  useUIStore.setState({ activeView: "library", libraryLoupe: false, isFullScreen: false, activePanel: Panel.Adjustments });
  workspace();
  expect(prepared()).toEqual([]);
});

const stills = () =>
  (invoke.mock.calls as unknown[][]).filter(([command]) => command === "prepare_embedded_stills").map(([, args]) => args);

it("asks for the next photographs' camera JPEGs at each step, before the decode", () => {
  useEditorStore.setState({
    selectedImage: { path: "/qa/b.dng", width: 0, height: 0, isRaw: true, isReady: false, isDecoded: false } as unknown as SelectedImage,
  });
  useUIStore.setState({ activeView: "editor", libraryLoupe: false, isFullScreen: false, activePanel: Panel.Adjustments });
  workspace();
  // Stepping faster than the decode prepares no proxies, but the stills.
  expect(prepared()).toEqual([]);
  expect(stills()).toEqual([{ paths: ["/qa/c.dng", "/qa/d.dng", "/qa/a.dng"] }]);
});

it("asks for no camera JPEGs in the Library's grid", () => {
  useUIStore.setState({ activeView: "library", libraryLoupe: false, isFullScreen: false, activePanel: Panel.Adjustments });
  workspace();
  expect(stills()).toEqual([]);
});
