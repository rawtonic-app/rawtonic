import "../../i18n";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import StudioDevelop from "./StudioDevelop";
import { useEditorStore } from "../../store/useEditorStore";
import { useUIStore } from "../../store/useUIStore";
import { debouncedSetHistory } from "../../hooks/useEditorActions";
import { INITIAL_ADJUSTMENTS } from "../../utils/adjustments";
import { COMMIT_INTERVAL_MS, registerSliderDraftRenderer } from "../../utils/sliderDraft";
import { Panel, type SelectedImage } from "../ui/AppProperties";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn((name: string) => Promise.resolve(name === "load_metadata" ? {} : undefined)),
}));

// Render counters in other tools: the LUT row in Effects and the dock's
// Snapshots (another tool column).
const renders = vi.hoisted(() => ({ lut: 0, snapshots: 0 }));
vi.mock("./StudioLut", () => ({
  default: () => {
    renders.lut++;
    return null;
  },
}));
vi.mock("./StudioSnapshots", () => ({
  default: () => {
    renders.snapshots++;
    return null;
  },
}));

let stopDrafts = () => {};
beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal("requestAnimationFrame", (fn: FrameRequestCallback) => setTimeout(() => fn(0), 16));
  vi.stubGlobal("cancelAnimationFrame", clearTimeout);
  vi.stubGlobal("PointerEvent", MouseEvent);
  useEditorStore.setState({
    ...useEditorStore.getInitialState(),
    adjustments: structuredClone(INITIAL_ADJUSTMENTS),
    selectedImage: { path: "/qa/photo.dng", width: 6000, height: 4000, isRaw: true, isReady: true } as SelectedImage,
  });
  useUIStore.setState({ activePanel: Panel.Adjustments });
  // The preview pipeline's draft renderer (useImageProcessing).
  stopDrafts = registerSliderDraftRenderer(() => true);
  renders.lut = 0;
  renders.snapshots = 0;
});
afterEach(() => {
  stopDrafts();
  cleanup();
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
  fireEvent.change(slider, { target: { value: String(value) } });
  act(() => vi.advanceTimersByTime(1000 / 60));
};

describe("Develop renders during a slider drag", () => {
  it("re-renders only the dragged tool on the drag's commits, all tools when it ends", () => {
    render(<StudioDevelop />);
    openGroups();
    const slider = screen.getByRole("slider", { name: "Exposure" });
    const before = { ...renders };
    expect(before.lut).toBeGreaterThan(0);
    expect(before.snapshots).toBeGreaterThan(0);

    fireEvent.pointerDown(slider, { button: 0 });
    for (let i = 1; i <= 30; i++) move(slider, i / 10);
    act(() => vi.advanceTimersByTime(COMMIT_INTERVAL_MS));
    // The drag committed (twice in 0.5 s) while other tools kept their render.
    expect(useEditorStore.getState().adjustments.exposure).toBe(3);
    expect(renders).toEqual(before);

    fireEvent.pointerUp(slider, { button: 0 });
    expect(useEditorStore.getState().isSliderDragging).toBe(false);
    expect(renders.lut).toBeGreaterThan(before.lut);
    expect(renders.snapshots).toBeGreaterThan(before.snapshots);
  });

  it("keeps displays linked to the dragged value following its commits", () => {
    render(<StudioDevelop />);
    fireEvent.click(within(screen.getByRole("navigation")).getByRole("button", { name: "Detail" }));
    openGroups();
    const detail = () => screen.getByRole("slider", { name: "Luminance noise detail" }) as HTMLInputElement;
    expect(detail().disabled).toBe(true);
    const luminance = screen.getAllByRole("slider", { name: "Luminance" })[0];
    fireEvent.pointerDown(luminance, { button: 0 });
    move(luminance, 20);
    act(() => vi.advanceTimersByTime(COMMIT_INTERVAL_MS));
    // The refinement under the amount enables with the drag's commit.
    expect(useEditorStore.getState().isSliderDragging).toBe(true);
    expect(detail().disabled).toBe(false);
    fireEvent.pointerUp(luminance, { button: 0 });
  });

  it("freezes no tool for a drag that did not start in one", () => {
    render(<StudioDevelop />);
    const before = { ...renders };
    // A targeted adjustment drag on the photo (TargetedAdjustmentOverlay).
    fireEvent.pointerDown(document.body, { button: 0 });
    act(() => useEditorStore.setState({ isSliderDragging: true }));
    act(() =>
      useEditorStore.setState((state) => ({
        adjustments: { ...state.adjustments, vignetteAmount: 10 },
      })),
    );
    expect(renders.lut).toBeGreaterThan(before.lut);
    expect(renders.snapshots).toBeGreaterThan(before.snapshots);
    act(() => useEditorStore.setState({ isSliderDragging: false }));
  });
});
