import { act, fireEvent, render } from "@testing-library/react";
import { createRef } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import Editor, { type TransformWrapperHandle } from "./Editor";
import { useEditorStore } from "../../store/useEditorStore";

// The viewer's pixels and gestures are not under test: only how a zoom
// tells the preview the size it ends at.
vi.mock("./editor/ImageCanvas", () => ({ default: () => null }));
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
    useEditorStore.setState({
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
});
