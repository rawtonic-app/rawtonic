import { act, cleanup, fireEvent, render } from "@testing-library/react";
import "../../i18n";
import { createRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import Editor, { type TransformWrapperHandle } from "./Editor";
import { useEditorStore } from "../../store/useEditorStore";
import { useUIStore } from "../../store/useUIStore";
import { INITIAL_ADJUSTMENTS } from "../../utils/adjustments";

// Keep the real viewer and targeted pointer surface; replace the renderer.
// Zoom tests also check the preview size requested at animation start.
vi.mock("./editor/ImageCanvas", async () => {
  const { default: TargetedAdjustmentOverlay } = await import("./editor/TargetedAdjustmentOverlay");
  // The frame list the preview parks (`switchPreviewLayers`).
  return { default: () => useEditorStore.getState().tatTarget
    ? <TargetedAdjustmentOverlay rect={{ left: 0, top: 0, width: 400, height: 300 }} /> : null };
});
// The viewer's fit; a test changes it as the viewer's size would change.
const fit = vi.hoisted(() => ({
  width: 400,
  height: 300,
  scale: 0.1,
  offsetX: 0,
  offsetY: 0,
  containerWidth: 400,
  containerHeight: 300,
  margin: 0,
}));
vi.mock("../../hooks/useImageRenderSize", () => ({
  useImageRenderSize: () => ({ ...fit }),
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
    // Unmount while the stubs still stand, so no React work outlives the file's jsdom.
    cleanup();
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

  describe("camera preview (Shift+\\)", () => {
    const rotated = { ...INITIAL_ADJUSTMENTS, rotation: 5 };

    beforeEach(() => {
      useEditorStore.setState({
        selectedImage: { path: "/a.raw", width: 4000, height: 3000, isReady: true, isRaw: true } as never,
        adjustments: rotated,
        cameraPreview: null,
      });
      // Develop, where the camera preview can be on (its guard ends it elsewhere).
      useUIStore.setState({ activeView: "editor", activePanel: null });
    });

    /** Cmd-double-click (resets crop and straighten), Shift-drag
     * (straightens), a drag (pans) and a click and pinch (zoom). */
    function editGestures(target: Element) {
      act(() => {
        for (let i = 0; i < 2; i++) {
          fireEvent.pointerDown(target, { button: 0, metaKey: true, clientX: 60, clientY: 60 });
          fireEvent.pointerUp(target, { button: 0, metaKey: true, clientX: 60, clientY: 60 });
        }
      });
      act(() => {
        fireEvent.pointerDown(target, { button: 0, shiftKey: true, clientX: 50, clientY: 50 });
        fireEvent.pointerMove(target, { button: 0, shiftKey: true, clientX: 250, clientY: 120 });
        fireEvent.pointerUp(target, { button: 0, shiftKey: true, clientX: 250, clientY: 120 });
      });
      act(() => {
        fireEvent.pointerDown(target, { button: 0, clientX: 50, clientY: 50 });
        fireEvent.pointerMove(target, { button: 0, clientX: 150, clientY: 120 });
        fireEvent.pointerUp(target, { button: 0, clientX: 150, clientY: 120 });
        fireEvent.click(target, { button: 0, clientX: 150, clientY: 120 });
        fireEvent.wheel(target, { deltaY: -100, clientX: 100, clientY: 100, ctrlKey: true });
      });
    }

    it("leaves the hidden edit inert: no Cmd-double-click reset, Shift-drag straighten, pan or zoom", () => {
      const ref = createRef<TransformWrapperHandle>();
      const { container } = render(<Editor onContextMenu={() => {}} transformWrapperRef={ref} />);
      act(() => useEditorStore.getState().setEditor({ cameraPreview: { path: "/a.raw" } }));
      const blocker = container.querySelector(".studio-camera-preview-blocker")!;
      expect(blocker).toBeTruthy();
      expect(container.querySelector('[data-bench-id="camera-preview"]')?.textContent).toContain("Camera preview");
      const transform = { ...ref.current!.instance.transformState };
      editGestures(blocker);
      expect(useEditorStore.getState().adjustments).toBe(rotated);
      expect(ref.current!.instance.transformState).toEqual(transform);
      // The same gestures reach the edit once it is back (the test sees them).
      act(() => useEditorStore.getState().setEditor({ cameraPreview: null }));
      expect(container.querySelector(".studio-camera-preview-blocker")).toBeNull();
      editGestures(ref.current!.instance.wrapperComponent!);
      expect(useEditorStore.getState().adjustments.rotation).not.toBe(5);
    });

    it("places the camera JPEG natively in the viewer to fit, whatever the edit's zoom", async () => {
      vi.useRealTimers();
      vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => setTimeout(() => callback(0), 0));
      vi.stubGlobal("cancelAnimationFrame", (id: number) => clearTimeout(id));
      vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockReturnValue(
        { left: 10, top: 20, width: 400, height: 300, right: 410, bottom: 320, x: 10, y: 20 } as DOMRect,
      );
      useEditorStore.setState({ hasRenderedFirstFrame: true });
      const transforms = () =>
        vi.mocked(invoke).mock.calls
          .filter(([command]) => command === "update_wgpu_transform")
          .map(([, args]) => (args as { payload: { width: number; viewerArea: number[] } }).payload);
      const ref = createRef<TransformWrapperHandle>();
      const { unmount } = render(<Editor onContextMenu={() => {}} transformWrapperRef={ref} />);
      await vi.waitFor(() => expect(transforms().length).toBeGreaterThan(0));
      const dpr = window.devicePixelRatio || 1;
      const area = [10 * dpr, 20 * dpr, 400 * dpr, 300 * dpr];
      expect(transforms().at(-1)!.viewerArea).toEqual(area);
      // Zoomed in, the edit's frame is larger than the viewer; the camera
      // preview's area (where the native display fits it) stays the viewer.
      act(() => ref.current!.setTransform(0, 0, 3));
      await vi.waitFor(() => expect(transforms().at(-1)!.width).toBeCloseTo(1200 * dpr));
      act(() => useEditorStore.getState().setEditor({ cameraPreview: { path: "/a.raw" } }));
      await new Promise((resolve) => setTimeout(resolve, 20));
      expect(transforms().at(-1)!.viewerArea).toEqual(area);
      // No frame or sync left running after the test (real timers).
      unmount();
      await new Promise((resolve) => setTimeout(resolve, 50));
    });
  });

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

  it("keeps the magnification and the centred detail when the before pane halves the viewer", () => {
    // 4000 × 3000 photo shown in an 800 × 300 viewer: fitted by height, 400 × 300 centred.
    Object.assign(fit, { width: 400, height: 300, offsetX: 200, offsetY: 0, containerWidth: 800, containerHeight: 300 });
    const size = (dim: "clientWidth" | "clientHeight") =>
      Object.defineProperty(HTMLElement.prototype, dim, { configurable: true, get: () => (dim === "clientWidth" ? fit.containerWidth : fit.containerHeight) });
    size("clientWidth");
    size("clientHeight");
    const ref = createRef<TransformWrapperHandle>();
    const view = render(<Editor onContextMenu={() => {}} transformWrapperRef={ref} />);
    // Zoom to 3x with the photo point (0.25, 0.5) at the viewer's centre.
    const [a, b] = [0.25, 0.5];
    act(() => ref.current!.setTransform(400 - 3 * (200 + a * 400), 150 - 3 * (0 + b * 300), 3));
    expect(ref.current!.instance.transformState.scale).toBe(3);
    // The before pane opens: the layout flips first, the viewer's new size arrives after it.
    act(() => useEditorStore.setState({ splitView: true, splitOrientation: "side-by-side" } as never));
    Object.assign(fit, { width: 400, height: 150, offsetX: 0, offsetY: 0, containerWidth: 400, containerHeight: 150 });
    act(() => view.rerender(<Editor onContextMenu={() => {}} transformWrapperRef={ref} />));
    const { scale, positionX, positionY } = ref.current!.instance.transformState;
    // CSS pixels per photo pixel stay: 400 px wide before, 400 px wide now (the fit's width is unchanged).
    expect(scale).toBeCloseTo(3);
    // The same photo point still sits at the new viewer's centre.
    expect((fit.containerWidth / 2 - positionX) / scale / fit.width).toBeCloseTo(a);
    expect((fit.containerHeight / 2 - positionY) / scale / fit.height).toBeCloseTo(b);
    // The pane closes again: back to the first framing.
    act(() => useEditorStore.setState({ splitView: false } as never));
    Object.assign(fit, { width: 400, height: 300, offsetX: 200, offsetY: 0, containerWidth: 800, containerHeight: 300 });
    act(() => view.rerender(<Editor onContextMenu={() => {}} transformWrapperRef={ref} />));
    const back = ref.current!.instance.transformState;
    expect(back.scale).toBeCloseTo(3);
    expect((400 - back.positionX) / back.scale - 200).toBeCloseTo(a * 400);
    expect((150 - back.positionY) / back.scale).toBeCloseTo(b * 300);
    delete (HTMLElement.prototype as { clientWidth?: number }).clientWidth;
    delete (HTMLElement.prototype as { clientHeight?: number }).clientHeight;
  });
});
