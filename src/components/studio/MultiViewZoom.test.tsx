import "../../i18n";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import MultiViewStage from "./MultiViewStage";
import { useEditorStore } from "../../store/useEditorStore";
import { useLibraryStore } from "../../store/useLibraryStore";
import { useMultiViewStore } from "../../store/useMultiViewStore";
import { useSettingsStore } from "../../store/useSettingsStore";
import { useUIStore } from "../../store/useUIStore";
import { useSessionBar } from "../session/sessionBar";
import { Invokes, type AppSettings, type ImageFile } from "../ui/AppProperties";
import type { SharedZoom } from "../../utils/multiView";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(() => Promise.resolve(true)) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(() => Promise.resolve(() => {})) }));
vi.mock("react-toastify", () => ({ toast: { error: vi.fn(), success: vi.fn() } }));

const file = (path: string): ImageFile => ({
  path,
  is_edited: false,
  modified: 0,
  rating: 0,
  tags: null,
  exif: null,
  is_virtual_copy: false,
  is_cloud_placeholder: false,
  is_raw: true,
  group_id: null,
});
const paths = ["/s/a.CR3", "/s/b.CR3", "/s/c.CR3"];
const files = paths.map(file);
const SIZE = { width: 6000, height: 4000 };

let frames: FrameRequestCallback[] = [];
const runFrame = () => act(() => frames.splice(0).forEach((frame) => frame(0)));
const transform = { scale: 1, positionX: 0, positionY: 0 };
const editorHandle = {
  current: {
    resetTransform: vi.fn(() => Object.assign(transform, { scale: 1, positionX: 0, positionY: 0 })),
    setTransform: vi.fn((positionX: number, positionY: number, scale: number) =>
      Object.assign(transform, { scale, positionX, positionY }),
    ),
    instance: { transformState: transform },
  },
};

const zoomCalls = () =>
  vi
    .mocked(invoke)
    .mock.calls.filter(([command]) => command === Invokes.SetMultiViewZoom)
    .map(([, args]) => args as { zoom: SharedZoom | null; cells: Record<string, SharedZoom> | null });

function stage() {
  const view = render(
    <MultiViewStage
      files={files}
      onOpen={(path) => open(path)}
      onRate={vi.fn()}
      transformWrapperRef={editorHandle as never}
    >
      <div data-testid="editor" />
    </MultiViewStage>,
  );
  runFrame();
  return view;
}

function open(path: string) {
  useEditorStore.setState({
    selectedImage: { path, isReady: true, width: SIZE.width, height: SIZE.height } as never,
  });
}

/** A secondary cell's press area and its photograph area in window px. */
function cellOf(container: HTMLElement, path: string) {
  const cell = container.querySelector<HTMLElement>(`.studio-mv-cell[data-path="${path}"]`)!;
  const select = cell.querySelector<HTMLElement>(".studio-mv-select")!;
  const x = 100 + parseFloat(cell.style.left);
  const y = 50 + parseFloat(cell.style.top);
  const width = parseFloat(cell.style.width);
  const height = parseFloat(select.style.height);
  return { cell, select, center: { clientX: x + width / 2, clientY: y + height / 2 } };
}

const badge = (container: HTMLElement, path: string) =>
  container.querySelector(`.studio-mv-cell[data-path="${path}"] .studio-mv-zoom`)?.textContent ?? null;

beforeEach(() => {
  vi.clearAllMocks();
  frames = [];
  Object.assign(transform, { scale: 1, positionX: 0, positionY: 0 });
  vi.stubGlobal("requestAnimationFrame", (cb: FrameRequestCallback) => frames.push(cb));
  vi.stubGlobal("cancelAnimationFrame", () => {});
  vi.stubGlobal(
    "ResizeObserver",
    class {
      observe() {}
      disconnect() {}
    },
  );
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockReturnValue({
    left: 100,
    top: 50,
    width: 1200,
    height: 800,
    right: 1300,
    bottom: 850,
    x: 100,
    y: 50,
    toJSON: () => ({}),
  });
  Object.defineProperty(window, "devicePixelRatio", { value: 2, configurable: true });
  useSettingsStore.setState({ appSettings: { keybinds: {} } as unknown as AppSettings, osPlatform: "macos" });
  useUIStore.setState({ activeView: "editor" });
  useMultiViewStore.setState({
    enabled: true,
    editAll: false,
    unavailable: false,
    active: false,
    reference: null,
    zoomLock: false,
    cellZooms: {},
    cellSizes: Object.fromEntries(paths.map((path) => [path, SIZE])),
  });
  useLibraryStore.setState({ multiSelectedPaths: paths, imageRatings: {}, imageFlags: {} });
  useSessionBar.setState({ mark: null });
  useEditorStore.setState({
    baseRenderSize: { width: 600, height: 400, offsetX: 0, offsetY: 50, containerWidth: 600, containerHeight: 500 },
    adjustments: { ...useEditorStore.getState().adjustments, crop: null, orientationSteps: 0 },
  });
  open("/s/b.CR3");
});
afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe("individual zoom (zoom lock off)", () => {
  it("zooms only the cell under the wheel, which keeps its zoom", () => {
    const { container } = stage();
    const a = cellOf(container, "/s/a.CR3");
    fireEvent.wheel(a.select, { deltaY: -400, ...a.center });
    runFrame();
    const zoom = useMultiViewStore.getState().cellZooms["/s/a.CR3"];
    expect(zoom.scale).toBeGreaterThan(0.2);
    // Only that cell is zoomed natively; the others (and the primary) stay.
    expect(zoomCalls().at(-1)).toEqual({ zoom: null, cells: { "/s/a.CR3": zoom } });
    expect(badge(container, "/s/a.CR3")).toMatch(/%/);
    expect(badge(container, "/s/c.CR3")).toBeNull();
    expect(badge(container, "/s/b.CR3")).toBeNull();
    expect(transform.scale).toBe(1);
    // Wheeling the other way returns it to fit.
    fireEvent.wheel(a.select, { deltaY: 4000, ...a.center });
    runFrame();
    expect(useMultiViewStore.getState().cellZooms["/s/a.CR3"]).toBeUndefined();
    expect(zoomCalls().at(-1)).toEqual({ zoom: null, cells: {} });
  });

  it("zooms the hovered cell with the zoom keys and shows 100 %", () => {
    const { container } = stage();
    const c = cellOf(container, "/s/c.CR3");
    fireEvent.pointerEnter(c.select, c.center);
    let handled = false;
    act(() => {
      handled = useMultiViewStore.getState().zoomHoveredCell!("cycle");
    });
    expect(handled).toBe(true);
    expect(useMultiViewStore.getState().cellZooms["/s/c.CR3"].scale).toBeCloseTo(1);
    expect(badge(container, "/s/c.CR3")).toBe("100%");
    act(() => {
      useMultiViewStore.getState().zoomHoveredCell!("fit");
    });
    expect(badge(container, "/s/c.CR3")).toBeNull();
    // Not over another cell: the keys go to the primary.
    fireEvent.pointerLeave(c.select);
    expect(useMultiViewStore.getState().zoomHoveredCell!("cycle")).toBe(false);
  });

  it("pans a zoomed cell with a drag, without making it the primary", () => {
    const { container } = stage();
    const a = cellOf(container, "/s/a.CR3");
    fireEvent.pointerEnter(a.select, a.center);
    act(() => {
      useMultiViewStore.getState().zoomHoveredCell!("actual");
    });
    const before = useMultiViewStore.getState().cellZooms["/s/a.CR3"];
    fireEvent.pointerDown(a.select, { button: 0, pointerId: 1, ...a.center });
    fireEvent.pointerMove(a.select, { pointerId: 1, clientX: a.center.clientX + 40, clientY: a.center.clientY });
    fireEvent.pointerUp(a.select, { pointerId: 1 });
    fireEvent.click(a.select);
    const after = useMultiViewStore.getState().cellZooms["/s/a.CR3"];
    expect(after.scale).toBe(before.scale);
    // The photograph moved right with the pointer: the centre moved left.
    expect(after.center[0]).toBeLessThan(before.center[0]);
    expect(useEditorStore.getState().selectedImage?.path).toBe("/s/b.CR3");
    // A plain click still makes it the primary.
    fireEvent.click(a.select);
    expect(useEditorStore.getState().selectedImage?.path).toBe("/s/a.CR3");
  });

  it("hands each photograph its own zoom when it becomes or stops being the primary", () => {
    const { container, rerender } = stage();
    // The primary is zoomed in the editor (5 × fit = 1:1, centred).
    Object.assign(transform, { scale: 5, positionX: 300 - 300 * 5, positionY: 250 - 250 * 5 });
    runFrame();
    expect(badge(container, "/s/b.CR3")).toBe("100%");
    // Cell a was zoomed on its own.
    useMultiViewStore.getState().setCellZoom("/s/a.CR3", { scale: 0.5, center: [0.25, 0.5] });
    act(() => open("/s/a.CR3"));
    rerender(
      <MultiViewStage files={files} onOpen={(path) => open(path)} onRate={vi.fn()} transformWrapperRef={editorHandle as never}>
        <div data-testid="editor" />
      </MultiViewStage>,
    );
    runFrame();
    // b keeps 1:1 as its cell's zoom; the editor shows a's own zoom.
    const zooms = useMultiViewStore.getState().cellZooms;
    expect(zooms["/s/b.CR3"].scale).toBeCloseTo(1);
    expect(editorHandle.current.setTransform).toHaveBeenCalled();
    expect(transform.scale).toBeCloseTo((0.5 * 6000) / (600 * 2));
    runFrame();
    expect(zoomCalls().at(-1)!.cells!["/s/b.CR3"].scale).toBeCloseTo(1);
    expect(zoomCalls().at(-1)!.cells!["/s/a.CR3"]).toBeUndefined();
    // A photograph without a zoom of its own opens at fit.
    act(() => open("/s/c.CR3"));
    rerender(
      <MultiViewStage files={files} onOpen={(path) => open(path)} onRate={vi.fn()} transformWrapperRef={editorHandle as never}>
        <div data-testid="editor" />
      </MultiViewStage>,
    );
    expect(editorHandle.current.resetTransform).toHaveBeenCalled();
    expect(useMultiViewStore.getState().cellZooms["/s/a.CR3"].scale).toBeCloseTo(0.5);
  });
});

describe("zoom lock switching", () => {
  it("keeps what every cell shows when unlocked, and follows the primary again when locked", () => {
    useMultiViewStore.setState({ zoomLock: true });
    const { container } = stage();
    Object.assign(transform, { scale: 5, positionX: 300 - 300 * 5, positionY: 250 - 250 * 5 });
    runFrame();
    const shared = zoomCalls().at(-1)!;
    expect(shared.cells).toBeNull();
    expect(shared.zoom!.scale).toBeCloseTo(1);
    expect(badge(container, "/s/a.CR3")).toBe("100%");
    // Unlocked: each other cell keeps the shared zoom as its own.
    act(() => useMultiViewStore.getState().setZoomLock(false));
    runFrame();
    const own = zoomCalls().at(-1)!;
    expect(own.zoom).toBeNull();
    expect(Object.keys(own.cells!).sort()).toEqual(["/s/a.CR3", "/s/c.CR3"]);
    expect(own.cells!["/s/a.CR3"].scale).toBeCloseTo(1);
    // A wheel now zooms one cell only.
    const a = cellOf(container, "/s/a.CR3");
    fireEvent.wheel(a.select, { deltaY: 300, ...a.center });
    runFrame();
    const after = zoomCalls().at(-1)!.cells!;
    expect(after["/s/a.CR3"].scale).toBeLessThan(1);
    expect(after["/s/c.CR3"].scale).toBeCloseTo(1);
    // Locked again: every cell shows the primary's zoom, own zooms dropped.
    act(() => useMultiViewStore.getState().setZoomLock(true));
    runFrame();
    expect(useMultiViewStore.getState().cellZooms).toEqual({});
    const locked = zoomCalls().at(-1)!;
    expect(locked.cells).toBeNull();
    expect(locked.zoom!.scale).toBeCloseTo(1);
    // With the lock, the wheel over another cell is left alone.
    fireEvent.wheel(a.select, { deltaY: -300, ...a.center });
    expect(useMultiViewStore.getState().cellZooms).toEqual({});
  });

  it("toggles the lock with Shift+N", async () => {
    const { KEYBIND_DEFINITIONS } = await import("../../utils/keyboardUtils");
    const lock = KEYBIND_DEFINITIONS.find((d) => d.action === "toggle_zoom_lock");
    expect(lock?.defaultCombo).toEqual(["shift", "KeyN"]);
    const taken = KEYBIND_DEFINITIONS.filter(
      (d) => d.action !== "toggle_zoom_lock" && JSON.stringify(d.defaultCombo) === JSON.stringify(["shift", "KeyN"]),
    );
    expect(taken).toEqual([]);
  });
});

describe("lock toggle", () => {
  it("names its shortcut and the gestures in its tooltip", async () => {
    const { default: MultiViewButtons } = await import("./MultiViewButtons");
    useMultiViewStore.setState({ active: true, zoomLock: false });
    const { getByLabelText } = render(<MultiViewButtons />);
    const title = getByLabelText("Lock zoom").getAttribute("title")!;
    expect(title).toMatch(/\((Shift \+ N|⇧ ?N)\)$/);
    expect(title).toMatch(/double-click/);
  });
});
