import "../../i18n";
import { useEffect } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, render, renderHook } from "@testing-library/react";
import MultiViewStage from "./MultiViewStage";
import { panelsShowReady, useEditorStore } from "../../store/useEditorStore";
import { useLibraryStore } from "../../store/useLibraryStore";
import { useMultiViewStore } from "../../store/useMultiViewStore";
import { useSettingsStore } from "../../store/useSettingsStore";
import { useUIStore } from "../../store/useUIStore";
import { useAppNavigation, type AppNavigationProps } from "../../hooks/useAppNavigation";
import { globalImageCache } from "../../utils/ImageLRUCache";
import { INITIAL_ADJUSTMENTS } from "../../utils/adjustments";
import type { AppSettings, HistogramData, ImageFile, SelectedImage } from "../ui/AppProperties";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(() => Promise.resolve(true)) }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn() }));
vi.mock("@tauri-apps/api/path", () => ({ homeDir: vi.fn() }));
vi.mock("react-toastify", () => ({ toast: { error: vi.fn(), success: vi.fn() } }));
vi.mock("../../hooks/useEditorActions", () => ({
  debouncedSave: { flush: vi.fn() },
  debouncedSetHistory: { cancel: vi.fn() },
  useEditorActions: () => ({ applyAdjustmentsToPaths: vi.fn() }),
}));

// Portrait photographs: a fallback to a landscape shape while the next one
// loads would pack the cells differently for a moment.
const W = 4000;
const H = 6000;
const paths = ["/s/a.CR3", "/s/b.CR3", "/s/c.CR3", "/s/d.CR3"];
const file = (path: string): ImageFile => ({
  path,
  is_edited: false,
  modified: 0,
  rating: 0,
  tags: null,
  exif: { PhotographicSensitivity: "200" },
  is_virtual_copy: false,
  is_cloud_placeholder: false,
  is_raw: true,
  group_id: null,
});
const files = paths.map(file);
const histogram = { red: [1], green: [1], blue: [1], luma: [1] } as unknown as HistogramData;
const ready = (path: string): SelectedImage =>
  ({ path, isReady: true, isDecoded: true, width: W, height: H, exif: {}, isRaw: true, thumbnailUrl: "" }) as SelectedImage;
const refs = () =>
  Object.fromEntries(
    [
      "transformWrapperRef",
      "cachedEditStateRef",
      "selectedImagePathRef",
      "isBackendReadyRef",
      "latestRenderedJobIdRef",
      "previewJobIdRef",
      "currentResRef",
      "prevAdjustmentsRef",
    ].map((key) => [key, { current: null }]),
  ) as unknown as AppNavigationProps["refs"];

let mounts = 0;
let unmounts = 0;
function EditorProbe() {
  useEffect(() => {
    mounts += 1;
    return () => {
      unmounts += 1;
    };
  }, []);
  return <div data-testid="editor" />;
}

/** Every store state between the steps, as the panels and the grid see it. */
interface Frame {
  panelsReady: boolean;
  open: boolean;
  histogram: boolean;
  selection: string;
  active: boolean;
}
let frames: Frame[] = [];
const frame = (): Frame => {
  const editor = useEditorStore.getState();
  return {
    panelsReady: panelsShowReady(editor),
    open: editor.selectedImage !== null,
    histogram: editor.histogram !== null,
    selection: useLibraryStore.getState().multiSelectedPaths.join(","),
    active: useMultiViewStore.getState().active,
  };
};

const layout = (container: HTMLElement) =>
  [...container.querySelectorAll<HTMLElement>(".studio-mv-cell")].map(
    (el) => `${el.dataset.path ?? "more"}@${el.style.left},${el.style.top},${el.style.width},${el.style.height}`,
  );

/** What the image loader publishes once the photograph's source is in. */
const publish = (path: string) =>
  act(() => {
    useEditorStore.setState((state) => ({
      selectedImage: { ...state.selectedImage!, ...ready(path) },
      adjustments: { ...INITIAL_ADJUSTMENTS },
      panelsHeld: false,
    }));
  });

let unsubscribe: (() => void)[] = [];
beforeEach(() => {
  vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
  mounts = 0;
  unmounts = 0;
  frames = [];
  globalImageCache.clear();
  vi.stubGlobal(
    "ResizeObserver",
    class {
      observe() {}
      disconnect() {}
    },
  );
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockReturnValue({
    left: 0,
    top: 0,
    width: 1600,
    height: 900,
    right: 1600,
    bottom: 900,
    x: 0,
    y: 0,
    toJSON: () => ({}),
  });
  useSettingsStore.setState({
    appSettings: { keybinds: {}, useWgpuRenderer: true } as unknown as AppSettings,
    osPlatform: "macos",
  });
  useUIStore.setState({ activeView: "editor" });
  useMultiViewStore.setState({ enabled: true, editAll: false, unavailable: false, active: false });
  useLibraryStore.setState({
    imageList: files,
    multiSelectedPaths: [...paths],
    libraryActivePath: paths[0],
    imageRatings: {},
    imageFlags: {},
  });
  useEditorStore.setState({
    selectedImage: ready(paths[0]),
    adjustments: { ...INITIAL_ADJUSTMENTS },
    histogram,
    hasRenderedFirstFrame: true,
    panelsHeld: false,
  });
  unsubscribe = [useEditorStore, useLibraryStore, useMultiViewStore].map((store) =>
    (store as { subscribe(listener: () => void): () => void }).subscribe(() => frames.push(frame())),
  );
});
afterEach(() => {
  unsubscribe.forEach((stop) => stop());
  cleanup();
  vi.useRealTimers();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

function setup() {
  const view = render(
    <MultiViewStage files={files} onOpen={vi.fn()} onRate={vi.fn()}>
      <EditorProbe />
    </MultiViewStage>,
  );
  const navigation = renderHook(() => useAppNavigation({ clearThumbnailQueue: vi.fn(), refs: refs() }));
  return { ...view, select: (path: string) => act(() => void navigation.result.current.handleImageSelect(path, true)) };
}

/** Nothing in between may show an empty, dimmed or single view state. */
function expectSteadyFrames() {
  expect(frames.length).toBeGreaterThan(0);
  for (const f of frames) {
    expect(f).toEqual({ panelsReady: true, open: true, histogram: true, selection: paths.join(","), active: true });
  }
}

describe("stepping the primary in the multi view", () => {
  it("keeps the panels, the selection and the cells steady while the next photograph loads", () => {
    const { container, select } = setup();
    expect(useMultiViewStore.getState().active).toBe(true);
    const before = layout(container);
    expect(before).toHaveLength(4);
    frames = [];

    select(paths[1]);
    // Loading: the old values stay (inert), nothing blinks.
    expect(useEditorStore.getState().selectedImage?.isReady).toBe(false);
    expect(useEditorStore.getState().panelsHeld).toBe(true);
    expect(layout(container)).toEqual(before);
    publish(paths[1]);
    expect(useEditorStore.getState().panelsHeld).toBe(false);
    expect(layout(container)).toEqual(before);
    expect(container.querySelector(".is-primary")?.getAttribute("data-path")).toBe(paths[1]);

    select(paths[2]);
    publish(paths[2]);
    expect(layout(container)).toEqual(before);

    expectSteadyFrames();
    // The live editor moved from cell to cell without being mounted again.
    expect(mounts).toBe(1);
    expect(unmounts).toBe(0);
  });

  it("keeps them steady for a photograph opened before (cached recipe)", () => {
    globalImageCache.set(paths[3], {
      selectedImage: ready(paths[3]),
      adjustments: { ...INITIAL_ADJUSTMENTS },
      histogram,
      waveform: null,
      finalPreviewUrl: null,
      uncroppedPreviewUrl: null,
      originalSize: { width: W, height: H },
      previewSize: { width: W, height: H },
    } as never);
    const { container, select } = setup();
    const before = layout(container);
    frames = [];
    select(paths[3]);
    expect(useEditorStore.getState().selectedImage?.isReady).toBe(false);
    expect(layout(container)).toEqual(before);
    publish(paths[3]);
    expect(layout(container)).toEqual(before);
    expectSteadyFrames();
    expect(mounts).toBe(1);
  });

  it("re-packs the cells when the primary itself is reshaped", () => {
    const { container } = setup();
    const before = layout(container);
    act(() =>
      useEditorStore.setState((s) => ({
        adjustments: { ...s.adjustments, crop: { x: 0, y: 0, width: 6000, height: 2000, unit: "px" } as never },
      })),
    );
    expect(layout(container)).not.toEqual(before);
  });

  it("shows the panels as not ready when the next photograph takes too long", () => {
    const { select } = setup();
    select(paths[1]);
    expect(panelsShowReady(useEditorStore.getState())).toBe(true);
    act(() => vi.advanceTimersByTime(2500));
    expect(panelsShowReady(useEditorStore.getState())).toBe(false);
  });

  it("does not hold the panels when no photograph was ready before", () => {
    useEditorStore.setState({ selectedImage: { ...ready(paths[0]), isReady: false } });
    const { select } = setup();
    select(paths[1]);
    expect(useEditorStore.getState().panelsHeld).toBe(false);
    expect(panelsShowReady(useEditorStore.getState())).toBe(false);
  });
});
