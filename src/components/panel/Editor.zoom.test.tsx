import { act, fireEvent, render } from "@testing-library/react";
import { createRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import Editor, { type TransformWrapperHandle } from "./Editor";
import { useEditorStore } from "../../store/useEditorStore";

// Keep the real viewer and targeted pointer surface; replace the renderer.
// Zoom tests also check the preview size requested at animation start.
vi.mock("./editor/ImageCanvas", async () => {
  const { default: TargetedAdjustmentOverlay } = await import("./editor/TargetedAdjustmentOverlay");
  return { default: () => useEditorStore.getState().tatTarget
    ? <TargetedAdjustmentOverlay rect={{ left: 0, top: 0, width: 400, height: 300 }} /> : null };
});
vi.mock("../../hooks/useImageRenderSize", () => ({
  useImageRenderSize: () => ({
    width: 400,
    height: 300,
    scale: 0.1,
    offsetX: 0,
    offsetY: 0,
    containerWidth: 400,
    containerHeight: 300,
    margin: 0,
  }),
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(() => Promise.resolve(null)) }));

describe("Editor zoom", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    // The animation's frames never run: what is reported comes from its start.
    vi.stubGlobal("requestAnimationFrame", () => 1);
    vi.stubGlobal("cancelAnimationFrame", () => {});
    vi.stubGlobal(
      "ResizeObserver",
      class {
        observe() {}
        unobserve() {}
        disconnect() {}
      },
    );
    // jsdom has no pointer capture.
    Object.assign(HTMLElement.prototype, {
      setPointerCapture: () => {},
      releasePointerCapture: () => {},
      hasPointerCapture: () => false,
    });
    vi.stubGlobal("PointerEvent", class extends MouseEvent {
      pointerId = 1;
      pointerType = "mouse";
    });
    useEditorStore.setState({
      tatTarget: null,
      isWbPickerActive: false,
      colorPickerTarget: null,
      selectedImage: { path: "/a.raw", width: 4000, height: 3000, isReady: true } as never,
      displaySize: { width: 400, height: 300 },
    });
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
  });

  it("asks for the frame of an animated zoom's end when the animation starts", () => {
    const ref = createRef<TransformWrapperHandle>();
    render(<Editor onContextMenu={() => {}} transformWrapperRef={ref} />);
    act(() => ref.current!.zoomIn(Math.log(4), 200));
    // Still at the start of the animation…
    expect(ref.current!.instance.transformState.scale).toBe(1);
    expect(ref.current!.instance.targetTransformState.scale).toBeCloseTo(4);
    // …the preview already knows the size it ends at, without waiting for
    // the animation and the display size's settle (about 0.35 s).
    expect(useEditorStore.getState().displaySize).toEqual({ width: 1600, height: 1200 });
  });

  it("forgets the animation's end when another transform replaces it", () => {
    const ref = createRef<TransformWrapperHandle>();
    render(<Editor onContextMenu={() => {}} transformWrapperRef={ref} />);
    act(() => ref.current!.zoomIn(Math.log(4), 200));
    act(() => ref.current!.setTransform(0, 0, 2));
    expect(ref.current!.instance.targetTransformState.scale).toBe(2);
    expect(ref.current!.instance.transformState.scale).toBe(2);
  });

  it("forgets the animation's end when a press stops it without moving", () => {
    const ref = createRef<TransformWrapperHandle>();
    render(<Editor onContextMenu={() => {}} transformWrapperRef={ref} />);
    act(() => ref.current!.zoomIn(Math.log(4), 200));
    const wrapper = ref.current!.instance.wrapperComponent!;
    // A middle-button press stops the animation where it is; nothing moves.
    act(() => {
      fireEvent.pointerDown(wrapper, { pointerType: "mouse", button: 1, pointerId: 1, clientX: 10, clientY: 10 });
      fireEvent.pointerUp(wrapper, { pointerType: "mouse", button: 1, pointerId: 1, clientX: 10, clientY: 10 });
    });
    expect(ref.current!.instance.targetTransformState).toEqual(ref.current!.instance.transformState);
  });
  it.each(["curve", "regionCurve", "hue", "saturation", "luminance", "grayMixer"] as const)(
    "gives %s the first drag without capturing, panning or click zooming",
    async (tatTarget) => {
      useEditorStore.setState({ tatTarget });
      const ref = createRef<TransformWrapperHandle>();
      const { container } = render(<Editor onContextMenu={() => {}} transformWrapperRef={ref} />);
      const wrapper = ref.current!.instance.wrapperComponent!;
      const overlay = container.querySelector('[data-testid="targeted-adjustment"]')!;
      const viewerCapture = vi.spyOn(wrapper, "setPointerCapture");
      const overlayCapture = vi.spyOn(overlay, "setPointerCapture");
      const initial = { ...ref.current!.instance.transformState };
      await act(async () => {
        fireEvent.pointerDown(overlay, { button: 0, clientX: 100, clientY: 100 });
        fireEvent.pointerMove(overlay, { button: 0, clientX: 150, clientY: 80 });
        fireEvent.pointerUp(overlay, { button: 0, clientX: 150, clientY: 80 });
        fireEvent.click(overlay);
      });
      expect(overlayCapture).toHaveBeenCalled();
      expect(invoke).toHaveBeenCalledWith(
        tatTarget === "curve" || tatTarget === "regionCurve" ? "sample_curve_input" : "sample_preview_white_balance",
        expect.objectContaining({ path: "/a.raw" }),
      );
      expect(viewerCapture).not.toHaveBeenCalled();
      expect(ref.current!.instance.transformState).toEqual(initial);
      expect(ref.current!.instance.targetTransformState).toEqual(initial);
      expect(wrapper.style.cursor).toBe("ns-resize");
    },
  );

  it("keeps middle-button pan available while targeting", () => {
    useEditorStore.setState({ tatTarget: "curve" });
    const ref = createRef<TransformWrapperHandle>();
    const { container } = render(<Editor onContextMenu={() => {}} transformWrapperRef={ref} />);
    const wrapper = ref.current!.instance.wrapperComponent!;
    const overlay = container.querySelector('[data-testid="targeted-adjustment"]')!;
    const capture = vi.spyOn(wrapper, "setPointerCapture");
    fireEvent.pointerDown(overlay, { button: 1, clientX: 100, clientY: 100 });
    expect(capture).toHaveBeenCalled();
    expect(wrapper.style.cursor).toBe("grabbing");
    fireEvent.pointerUp(overlay, { button: 1, clientX: 100, clientY: 100 });
    expect(wrapper.style.cursor).toBe("ns-resize");
  });

  it.each([false, true])("keeps wheel zoom available while targeting (trackpad pinch: %s)", (ctrlKey) => {
    useEditorStore.setState({ tatTarget: "curve" });
    const ref = createRef<TransformWrapperHandle>();
    render(<Editor onContextMenu={() => {}} transformWrapperRef={ref} />);
    const wrapper = ref.current!.instance.wrapperComponent!;
    fireEvent.wheel(wrapper, { deltaY: -100, clientX: 100, clientY: 100, ctrlKey });
    expect(ref.current!.instance.transformState.scale).toBeGreaterThan(1);
  });

});
