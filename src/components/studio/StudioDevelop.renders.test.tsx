import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { installRenderCensus, startRenderCensus, takeRenderCensus } from "../../utils/renderCensus";

// Renders per component are counted through React's DevTools hook, which
// goes in before react-dom loads: everything React is imported below.
installRenderCensus(globalThis);

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn((name: string) => Promise.resolve(name === "load_metadata" ? {} : undefined)),
}));

let rtl: typeof import("@testing-library/react");
let React: typeof import("react");
let StudioDevelop: typeof import("./StudioDevelop").default;
let StudioRelight: typeof import("./StudioRelight").default;
let StudioUpright: typeof import("./StudioUpright").default;
let StudioCropControls: typeof import("./StudioCropControls").default;
let ToolColumn: typeof import("./toolLayout").ToolColumn;
let ContextMenuProvider: typeof import("../../context/ContextMenuContext").ContextMenuProvider;
let useEditorStore: typeof import("../../store/useEditorStore").useEditorStore;
let debouncedSetHistory: typeof import("../../hooks/useEditorActions").debouncedSetHistory;
let sliderDraft: typeof import("../../utils/sliderDraft");
let INITIAL_ADJUSTMENTS: typeof import("../../utils/adjustments").INITIAL_ADJUSTMENTS;
let ImageProcessingManager: typeof import("../managers/ImageProcessingManager").default;
let useMultiViewStore: typeof import("../../store/useMultiViewStore").useMultiViewStore;
let useLibraryStore: typeof import("../../store/useLibraryStore").useLibraryStore;
let useUIStore: typeof import("../../store/useUIStore").useUIStore;
beforeAll(async () => {
  await import("../../i18n");
  rtl = await import("@testing-library/react");
  React = await import("react");
  StudioDevelop = (await import("./StudioDevelop")).default;
  StudioRelight = (await import("./StudioRelight")).default;
  StudioUpright = (await import("./StudioUpright")).default;
  StudioCropControls = (await import("./StudioCropControls")).default;
  ({ ToolColumn } = await import("./toolLayout"));
  ({ ContextMenuProvider } = await import("../../context/ContextMenuContext"));
  ({ useEditorStore } = await import("../../store/useEditorStore"));
  ({ debouncedSetHistory } = await import("../../hooks/useEditorActions"));
  sliderDraft = await import("../../utils/sliderDraft");
  ({ INITIAL_ADJUSTMENTS } = await import("../../utils/adjustments"));
  ImageProcessingManager = (await import("../managers/ImageProcessingManager")).default;
  ({ useMultiViewStore } = await import("../../store/useMultiViewStore"));
  ({ useLibraryStore } = await import("../../store/useLibraryStore"));
  ({ useUIStore } = await import("../../store/useUIStore"));
});

let stopDrafts = () => {};
beforeEach(async () => {
  vi.useFakeTimers();
  vi.stubGlobal("requestAnimationFrame", (fn: FrameRequestCallback) => setTimeout(() => fn(0), 16));
  vi.stubGlobal("cancelAnimationFrame", clearTimeout);
  vi.stubGlobal("PointerEvent", MouseEvent);
  const { Panel } = await import("../ui/AppProperties");
  useEditorStore.setState({
    ...useEditorStore.getInitialState(),
    adjustments: structuredClone(INITIAL_ADJUSTMENTS),
    selectedImage: { path: "/qa/photo.dng", width: 6000, height: 4000, isRaw: true, isReady: true } as never,
  });
  useUIStore.setState({ activePanel: Panel.Adjustments });
  // The preview pipeline's draft renderer (useImageProcessing).
  stopDrafts = sliderDraft.registerSliderDraftRenderer(() => true);
});
afterEach(() => {
  takeRenderCensus();
  useMultiViewStore.setState({ active: false, editAll: false });
  useLibraryStore.setState({ multiSelectedPaths: [] });
  useUIStore.setState({ activeView: "library" });
  stopDrafts();
  rtl.cleanup();
  debouncedSetHistory.cancel();
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

const openGroups = () =>
  document.querySelectorAll("details").forEach((group) => {
    group.open = true;
  });
/** Drag moves on a range input: the input event per move, as a browser sends. */
const move = (slider: HTMLElement, value: number) => {
  rtl.fireEvent.change(slider, { target: { value: String(value) } });
  rtl.act(() => vi.advanceTimersByTime(1000 / 60));
};
const setAdjustments = (patch: Record<string, unknown>) =>
  rtl.act(() => useEditorStore.setState((state) => ({ adjustments: { ...state.adjustments, ...patch } })));

describe("Develop renders during a slider drag", () => {
  it("re-renders only the dragged tool on the drag's commits, all tools when it ends", () => {
    rtl.render(<StudioDevelop />);
    const slider = rtl.screen.getByRole("slider", { name: "Exposure" });
    rtl.fireEvent.pointerDown(slider, { button: 0 });
    startRenderCensus();
    move(slider, 0.1);
    move(slider, 0.2);
    rtl.act(() => vi.advanceTimersByTime(sliderDraft.COMMIT_INTERVAL_MS));
    expect(useEditorStore.getState().adjustments.exposure).toBe(0.2);
    const during = takeRenderCensus();
    // One commit: the Exposure tool alone, its slider for each move and the
    // commit; the tools beside it, their own subscriptions included, not at all.
    expect(during.ToolGroup).toBe(1);
    expect(during.StudioToolCopy).toBe(1);
    expect(during.AdjustmentControl).toBe(3);
    for (const other of [
      "StudioLayers",
      "StudioPresets",
      "StudioProfileBrowser",
      "StudioWhiteBalance",
      "PointCurve",
      "RegionCurveControls",
      "StudioLevels",
      "StudioLut",
      "StudioRelight",
      "StudioSnapshots",
      "StudioHistory",
    ])
      expect([other, during[other] ?? 0]).toEqual([other, 0]);

    startRenderCensus();
    rtl.fireEvent.pointerUp(slider, { button: 0 });
    const after = takeRenderCensus();
    expect(useEditorStore.getState().isSliderDragging).toBe(false);
    // Tools that take their values from the panel show the drag's result.
    for (const tool of ["StudioWhiteBalance", "PointCurve", "StudioLevels", "StudioLut", "StudioSnapshots"])
      expect([tool, (after[tool] ?? 0) > 0]).toEqual([tool, true]);
  });

  it("keeps displays linked to the dragged value following its commits", () => {
    rtl.render(<StudioDevelop />);
    rtl.fireEvent.click(
      rtl.within(rtl.screen.getByRole("navigation")).getByRole("button", { name: "Detail" }),
    );
    openGroups();
    const detail = () => rtl.screen.getByRole("slider", { name: "Luminance noise detail" }) as HTMLInputElement;
    expect(detail().disabled).toBe(true);
    const luminance = rtl.screen.getAllByRole("slider", { name: "Luminance" })[0];
    rtl.fireEvent.pointerDown(luminance, { button: 0 });
    move(luminance, 20);
    rtl.act(() => vi.advanceTimersByTime(sliderDraft.COMMIT_INTERVAL_MS));
    // The refinement under the amount enables with the drag's commit.
    expect(useEditorStore.getState().isSliderDragging).toBe(true);
    expect(detail().disabled).toBe(false);
    rtl.fireEvent.pointerUp(luminance, { button: 0 });
  });

  it("freezes no tool for a drag that did not start in one", () => {
    rtl.render(<StudioDevelop />);
    // A targeted adjustment drag on the photo (TargetedAdjustmentOverlay).
    rtl.fireEvent.pointerDown(document.body, { button: 0 });
    rtl.act(() => useEditorStore.setState({ isSliderDragging: true }));
    startRenderCensus();
    setAdjustments({ vignetteAmount: 10 });
    const counts = takeRenderCensus();
    expect(counts.StudioLut).toBeGreaterThan(0);
    expect(counts.StudioSnapshots).toBeGreaterThan(0);
    rtl.act(() => useEditorStore.setState({ isSliderDragging: false }));
  });

  it("ends a press in a tool on release after another tool column went away", () => {
    // The library's column beside Develop's, hidden later (browse sidebar).
    const page = (library: boolean) => (
      <ContextMenuProvider>
        {library && (
          <ToolColumn scope="library">
            <div key="folders" />
          </ToolColumn>
        )}
        <StudioDevelop />
      </ContextMenuProvider>
    );
    const { rerender } = rtl.render(page(true));
    rerender(page(false));
    const slider = rtl.screen.getByRole("slider", { name: "Exposure" });
    rtl.fireEvent.pointerDown(slider, { button: 0 });
    rtl.fireEvent.pointerUp(slider, { button: 0 });
    // Quick Adjust's wheel over the photo then edits as a drag.
    rtl.act(() => useEditorStore.setState({ isSliderDragging: true }));
    startRenderCensus();
    setAdjustments({ vignetteAmount: 10 });
    expect(takeRenderCensus().StudioLut).toBeGreaterThan(0);
    rtl.act(() => useEditorStore.setState({ isSliderDragging: false }));
  });
});

describe("Tools with their own subscription", () => {
  it.each([
    ["StudioRelight", () => <StudioRelight disabled={false} onInteraction={() => {}} />, { relightOriginalLight: 50 }],
    ["StudioUpright", () => <StudioUpright />, { guidedPerspective: { enabled: true, lines: [], upright: "level" } }],
    ["StudioCropControls", () => <StudioCropControls />, { rotation: 5 }],
  ] as const)("%s re-renders for its own values, not for others", (name, element, own) => {
    rtl.render(React.createElement(element));
    startRenderCensus();
    setAdjustments({ exposure: 1, contrast: 20 });
    expect(takeRenderCensus()[name] ?? 0).toBe(0);
    startRenderCensus();
    setAdjustments(own);
    expect(takeRenderCensus()[name]).toBeGreaterThan(0);
  });
});

describe("Develop renders during a slider drag with 200 photographs selected", () => {
  // The multi view of the selection, with the preview pipeline's own draft
  // renderer (not the stand-in above).
  const develop = () => {
    const ref = <T,>(current: T) => ({ current });
    useLibraryStore.setState({
      multiSelectedPaths: Array.from({ length: 200 }, (_, i) => (i ? `/qa/photo-${i}.dng` : "/qa/photo.dng")),
    });
    useMultiViewStore.setState({ active: true });
    useUIStore.setState({ activeView: "editor" });
    rtl.render(
      <>
        <ImageProcessingManager
          transformWrapperRef={ref(null)}
          prevAdjustmentsRef={ref(null)}
          previewJobIdRef={ref(0)}
          latestRenderedJobIdRef={ref(0)}
          currentResRef={ref(1920)}
        />
        <StudioDevelop />
      </>,
    );
    return rtl.screen.getByRole("slider", { name: "Exposure" });
  };
  const drag = (slider: HTMLElement) => {
    rtl.fireEvent.pointerDown(slider, { button: 0 });
    startRenderCensus();
    for (let i = 1; i <= 12; i++) move(slider, i / 10);
    const during = takeRenderCensus();
    rtl.fireEvent.pointerUp(slider, { button: 0 });
    expect(useEditorStore.getState().adjustments.exposure).toBe(1.2);
    return during;
  };

  it("renders Develop and the preview hooks on the drag's commits, not on every move", () => {
    const during = drag(develop());
    // 12 moves over 200 ms: one store commit at most (COMMIT_INTERVAL_MS).
    expect(during.StudioDevelop ?? 0).toBeLessThanOrEqual(1);
    expect(during.ImageProcessingManager ?? 0).toBeLessThanOrEqual(1);
  });

  it("commits every move while Edit all selected is on", () => {
    useMultiViewStore.setState({ editAll: true });
    const during = drag(develop());
    expect(during.StudioDevelop).toBe(12);
  });
});
