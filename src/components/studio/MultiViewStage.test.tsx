import "../../i18n";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import MultiViewStage from "./MultiViewStage";
import { useEditorStore } from "../../store/useEditorStore";
import { useLibraryStore } from "../../store/useLibraryStore";
import { useMultiViewStore } from "../../store/useMultiViewStore";
import { useSettingsStore } from "../../store/useSettingsStore";
import { useUIStore } from "../../store/useUIStore";
import { useSessionBar } from "../session/sessionBar";
import { Invokes, type AppSettings, type ImageFile } from "../ui/AppProperties";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(() => Promise.resolve(true)),
}));
vi.mock("react-toastify", () => ({ toast: { error: vi.fn(), success: vi.fn() } }));

const file = (path: string): ImageFile => ({
  path,
  is_edited: false,
  modified: 0,
  rating: 0,
  tags: null,
  exif: { PhotographicSensitivity: "200", ExposureTime: "1/125", FNumber: "4" },
  is_virtual_copy: false,
  is_cloud_placeholder: false,
  is_raw: true,
  group_id: null,
});
const files = ["/s/a.CR3", "/s/b.CR3", "/s/c.CR3", "/s/d.CR3"].map(file);
const onOpen = vi.fn();
const onRate = vi.fn();

function open(path: string, selection: string[]) {
  useLibraryStore.setState({ multiSelectedPaths: selection });
  useEditorStore.setState({
    selectedImage: { path, isReady: true, width: 6000, height: 4000 } as never,
  });
}

function stage(list = files) {
  return render(
    <MultiViewStage files={list} onOpen={onOpen} onRate={onRate}>
      <div data-testid="editor" />
    </MultiViewStage>,
  );
}

const setMultiViewCalls = () =>
  vi.mocked(invoke).mock.calls.filter(([command]) => command === Invokes.SetMultiView);

beforeEach(() => {
  vi.clearAllMocks();
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
  useSettingsStore.setState({
    appSettings: { keybinds: {} } as unknown as AppSettings,
    osPlatform: "macos",
  });
  useUIStore.setState({ activeView: "editor" });
  useMultiViewStore.setState({ enabled: true, editAll: false, unavailable: false, active: false });
  useLibraryStore.setState({ imageRatings: {}, imageFlags: {} });
  useSessionBar.setState({ mark: null });
  open("/s/b.CR3", ["/s/b.CR3", "/s/a.CR3", "/s/c.CR3"]);
});
afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

const cells = (container: HTMLElement) =>
  [...container.querySelectorAll<HTMLElement>(".studio-mv-cell")].map((el) => el.dataset.path ?? "more");

describe("multi view", () => {
  it("shows every selected photograph in browser order, the primary framed", () => {
    const { container } = stage();
    expect(cells(container)).toEqual(["/s/a.CR3", "/s/b.CR3", "/s/c.CR3"]);
    expect(container.querySelector(".is-primary")?.getAttribute("data-path")).toBe("/s/b.CR3");
    expect(useMultiViewStore.getState().active).toBe(true);
    // The live editor sits in the primary's photograph area.
    const primary = container.querySelector<HTMLElement>(".studio-mv-primary")!;
    expect(primary.style.width).not.toBe("");
  });

  it("sends the native display each cell's photograph area in device pixels", () => {
    const { container } = stage();
    const [, args] = setMultiViewCalls().at(-1)!;
    const { primary, cells: sent } = args as {
      primary: string;
      cells: { path: string; area: number[] }[];
    };
    expect(primary).toBe("/s/b.CR3");
    expect(sent.map((c) => c.path)).toEqual(["/s/a.CR3", "/s/b.CR3", "/s/c.CR3"]);
    const first = container.querySelector<HTMLElement>(".studio-mv-cell")!;
    const left = parseFloat(first.style.left);
    const top = parseFloat(first.style.top);
    // Window coordinates (stage at 100, 50) times the pixel ratio.
    expect(sent[0].area[0]).toBeCloseTo((100 + left) * 2);
    expect(sent[0].area[1]).toBeCloseTo((50 + top) * 2);
    expect(sent[0].area[2]).toBeCloseTo(parseFloat(first.style.width) * 2);
  });

  it("shows one photograph alone and tells the display to leave the multi view", () => {
    const { container, rerender } = stage();
    expect(cells(container)).toHaveLength(3);
    act(() => open("/s/b.CR3", ["/s/b.CR3"]));
    rerender(
      <MultiViewStage files={files} onOpen={onOpen} onRate={onRate}>
        <div data-testid="editor" />
      </MultiViewStage>,
    );
    expect(cells(container)).toEqual([]);
    expect(setMultiViewCalls().at(-1)![1]).toEqual({ primary: null, cells: [] });
    expect(screen.getByTestId("editor")).toBeTruthy();
  });

  it("is not offered without the native display", () => {
    useSettingsStore.setState({ osPlatform: "linux" });
    const { container } = stage();
    expect(cells(container)).toEqual([]);
    expect(setMultiViewCalls()).toHaveLength(0);
  });

  it("stays off when switched off (N)", () => {
    useMultiViewStore.setState({ enabled: false });
    const { container } = stage();
    expect(cells(container)).toEqual([]);
  });

  it("frames every cell orange while Edit all selected is on", () => {
    const { container } = stage();
    expect(container.querySelectorAll(".is-edit-all")).toHaveLength(0);
    act(() => useMultiViewStore.setState({ editAll: true }));
    expect(container.querySelectorAll(".studio-mv-cell.is-edit-all")).toHaveLength(3);
  });

  it("switches Edit all selected off when the multi view ends", () => {
    stage();
    act(() => useMultiViewStore.setState({ editAll: true }));
    act(() => useLibraryStore.setState({ multiSelectedPaths: ["/s/b.CR3"] }));
    expect(useMultiViewStore.getState().editAll).toBe(false);
  });

  it("makes a clicked photograph the primary", () => {
    stage();
    fireEvent.click(screen.getByRole("button", { name: /Edit c\.CR3/ }));
    expect(onOpen).toHaveBeenCalledWith("/s/c.CR3");
  });

  it("removes a photograph from the selection from its hover controls", () => {
    stage();
    fireEvent.click(screen.getByRole("button", { name: "Remove c.CR3 from the selection" }));
    expect(useLibraryStore.getState().multiSelectedPaths).toEqual(["/s/a.CR3", "/s/b.CR3"]);
    expect(onOpen).not.toHaveBeenCalled();
  });

  it("hands over to the next photograph when the primary is removed", () => {
    stage();
    fireEvent.click(screen.getByRole("button", { name: "Remove b.CR3 from the selection" }));
    expect(onOpen).toHaveBeenCalledWith("/s/c.CR3");
    expect(useLibraryStore.getState().multiSelectedPaths).toEqual(["/s/a.CR3", "/s/c.CR3"]);
  });

  it("rates and flags the hovered photograph only", () => {
    const mark = vi.fn();
    useSessionBar.setState({ mark });
    stage();
    fireEvent.click(screen.getByRole("button", { name: "Rate a.CR3 3 stars" }));
    expect(onRate).toHaveBeenCalledWith(3, ["/s/a.CR3"]);
    fireEvent.click(screen.getByRole("button", { name: "Reject a.CR3" }));
    expect(mark).toHaveBeenCalledWith(-1, ["/s/a.CR3"]);
  });

  it("gives the last cell to '+N more' with more than twelve selected", () => {
    const many = Array.from({ length: 15 }, (_, i) => file(`/s/p${i}.CR3`));
    open("/s/p14.CR3", many.map((f) => f.path));
    const { container } = stage(many);
    const shown = cells(container);
    expect(shown).toHaveLength(12);
    expect(shown.at(-1)).toBe("more");
    expect(shown).toContain("/s/p14.CR3");
    expect(screen.getByText("and 4 more")).toBeTruthy();
  });

  it("captions grow with the cell: name and capture values", () => {
    useLibraryStore.setState({ imageRatings: { "/s/a.CR3": 2 } });
    const { container } = stage();
    const caption = container.querySelector('[data-path="/s/a.CR3"] .studio-mv-caption')!;
    expect(caption.textContent).toContain("a.CR3");
    // Three cells in 1200 px: medium captions (ISO, shutter, aperture, rating).
    expect(caption.textContent).toContain("ISO 200 · 1/125 s · f/4");
    expect(caption.textContent).toContain("★★");
  });
});

describe("reference and locked zoom", () => {
  it("shows the pinned reference first with its pin, outside the selection", () => {
    useMultiViewStore.setState({ reference: "/s/d.CR3" });
    const { container } = stage();
    expect(cells(container)).toEqual(["/s/d.CR3", "/s/a.CR3", "/s/b.CR3", "/s/c.CR3"]);
    const reference = container.querySelector('[data-path="/s/d.CR3"]')!;
    expect(reference.classList.contains("is-reference")).toBe(true);
    expect(screen.getByRole("img", { name: "Reference: d.CR3" })).toBeTruthy();
    // Not a "make primary" target; its × unpins instead of deselecting.
    expect(screen.queryByRole("button", { name: /Edit d\.CR3/ })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Unpin the reference d.CR3" }));
    expect(useMultiViewStore.getState().reference).toBeNull();
    useMultiViewStore.setState({ reference: null });
  });

  it("makes a 2-up compare with one other photograph", () => {
    useMultiViewStore.setState({ reference: "/s/d.CR3" });
    open("/s/b.CR3", ["/s/b.CR3"]);
    const { container } = stage();
    expect(cells(container)).toEqual(["/s/d.CR3", "/s/b.CR3"]);
    useMultiViewStore.setState({ reference: null });
  });

  it("keeps the reference out of Edit all selected", () => {
    useMultiViewStore.setState({ reference: "/s/d.CR3", editAll: true });
    const { container } = stage();
    expect(container.querySelector('[data-path="/s/d.CR3"]')!.classList.contains("is-edit-all")).toBe(false);
    expect(container.querySelectorAll(".is-edit-all")).toHaveLength(3);
    useMultiViewStore.setState({ reference: null, editAll: false });
  });

  function zoomed(lock: boolean) {
    const frames: FrameRequestCallback[] = [];
    vi.stubGlobal("requestAnimationFrame", (cb: FrameRequestCallback) => frames.push(cb));
    vi.stubGlobal("cancelAnimationFrame", () => {});
    useMultiViewStore.setState({ zoomLock: lock });
    useEditorStore.setState({
      baseRenderSize: { width: 600, height: 400, offsetX: 0, offsetY: 50, containerWidth: 600, containerHeight: 500 },
      adjustments: { ...useEditorStore.getState().adjustments, crop: null, orientationSteps: 0 },
    });
    const ref = {
      current: {
        instance: { transformState: { scale: 5, positionX: 300 - 300 * 5, positionY: 250 - 250 * 5 } },
      },
    } as never;
    render(
      <MultiViewStage files={files} onOpen={onOpen} onRate={onRate} transformWrapperRef={ref}>
        <div />
      </MultiViewStage>,
    );
    act(() => frames.shift()?.(0));
    return vi.mocked(invoke).mock.calls.filter(([command]) => command === Invokes.SetMultiViewZoom);
  }

  it("sends the primary's zoom to every cell with the lock on", () => {
    const calls = zoomed(true);
    const { zoom } = calls.at(-1)![1] as { zoom: { scale: number; center: number[] } };
    // 600 CSS px × 5 × 2 device px over the 6000 px photograph: 1:1.
    expect(zoom.scale).toBeCloseTo(1);
    expect(zoom.center[0]).toBeCloseTo(0.5);
    expect(zoom.center[1]).toBeCloseTo(0.5);
  });

  it("leaves the other cells at fit with the lock off", () => {
    const calls = zoomed(false);
    expect(calls.at(-1)![1]).toEqual({ zoom: null, cells: {} });
    useMultiViewStore.setState({ zoomLock: true });
  });
});
