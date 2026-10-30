import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
} from "@testing-library/react";
import {
  afterEach,
  beforeAll,
  beforeEach,
  describe,
  expect,
  it,
  vi,
} from "vitest";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

import TargetedAdjustmentOverlay from "./TargetedAdjustmentOverlay";
import {
  debouncedSetHistory,
  useEditorStore,
} from "../../../store/useEditorStore";
import { INITIAL_ADJUSTMENTS } from "../../../utils/adjustments";
import { linearFromSrgb } from "../../../utils/targetedAdjustment";

beforeAll(() => {
  // jsdom has no pointer capture.
  Element.prototype.setPointerCapture = () => {};
  Element.prototype.hasPointerCapture = () => false;
  Element.prototype.releasePointerCapture = () => {};
  vi.stubGlobal("requestAnimationFrame", (cb: FrameRequestCallback) => {
    cb(0);
    return 1;
  });
  vi.stubGlobal("cancelAnimationFrame", () => {});
});

beforeEach(() => {
  useEditorStore.setState({
    selectedImage: { path: "/s/a.NEF", isReady: true, isRaw: true } as never,
    adjustments: structuredClone(INITIAL_ADJUSTMENTS),
    tatTarget: "saturation",
    tatFocus: null,
    curveChannel: "luma",
    tatCurveMarker: null,
    isSliderDragging: false,
  });
  invoke.mockReset();
});
afterEach(cleanup);

const surface = () => {
  render(
    <TargetedAdjustmentOverlay
      rect={{ left: 0, top: 0, width: 400, height: 300 }}
    />,
  );
  const el = screen.getByTestId("targeted-adjustment");
  el.getBoundingClientRect = () =>
    ({ left: 0, top: 0, width: 400, height: 300 }) as DOMRect;
  return el;
};

describe("targeted adjustment surface", () => {
  it("samples the photo colour under the pointer and moves its mixer channels with the drag", async () => {
    invoke.mockResolvedValue(linearFromSrgb([90, 140, 220]));
    const el = surface();
    await act(async () => {
      fireEvent.pointerDown(el, {
        button: 0,
        clientX: 100,
        clientY: 200,
        pointerId: 1,
      });
    });
    expect(invoke).toHaveBeenCalledWith("sample_preview_white_balance", {
      path: "/s/a.NEF",
      normalizedX: 0.25,
      normalizedY: 200 / 300,
    });
    expect(useEditorStore.getState().isSliderDragging).toBe(true);
    expect(useEditorStore.getState().tatFocus).toBe("blues");
    fireEvent.pointerMove(el, { clientX: 100, clientY: 160, pointerId: 1 });
    fireEvent.pointerUp(el, { clientX: 100, clientY: 160, pointerId: 1 });
    const { adjustments, isSliderDragging } = useEditorStore.getState();
    expect(adjustments.hsl.blues.saturation).toBe(20);
    expect(isSliderDragging).toBe(false);
  });

  it("applies a flick that ends before the colour sample arrives", async () => {
    let resolve!: (value: unknown) => void;
    invoke.mockImplementation(() => new Promise((r) => (resolve = r)));
    const el = surface();
    fireEvent.pointerDown(el, {
      button: 0,
      clientX: 100,
      clientY: 200,
      pointerId: 1,
    });
    fireEvent.pointerMove(el, { clientX: 100, clientY: 180, pointerId: 1 });
    fireEvent.pointerUp(el, { clientX: 100, clientY: 180, pointerId: 1 });
    await act(async () => resolve(linearFromSrgb([90, 140, 220])));
    expect(useEditorStore.getState().adjustments.hsl.blues.saturation).toBe(10);
  });

  it("leaves the photo alone when grey is pressed", async () => {
    invoke.mockResolvedValue(linearFromSrgb([120, 120, 120]));
    const el = surface();
    await act(async () => {
      fireEvent.pointerDown(el, {
        button: 0,
        clientX: 10,
        clientY: 10,
        pointerId: 1,
      });
    });
    fireEvent.pointerMove(el, { clientX: 10, clientY: -90, pointerId: 1 });
    fireEvent.pointerUp(el, { pointerId: 1 });
    expect(useEditorStore.getState().adjustments.hsl).toEqual(
      INITIAL_ADJUSTMENTS.hsl,
    );
  });

  it("reads the level the tone curve receives", async () => {
    useEditorStore.setState({ tatTarget: "curve" });
    invoke.mockResolvedValue([linearFromSrgb([100, 100, 100])]);
    const el = surface();
    await act(async () => {
      fireEvent.pointerDown(el, {
        button: 0,
        clientX: 10,
        clientY: 100,
        pointerId: 1,
      });
    });
    expect(invoke).toHaveBeenCalledWith("sample_curve_input", {
      path: "/s/a.NEF",
      normalizedX: 10 / 400,
      normalizedY: 100 / 300,
      adjustments: useEditorStore.getState().adjustments,
    });
    fireEvent.pointerMove(el, { clientX: 10, clientY: 80, pointerId: 1 });
    fireEvent.pointerUp(el, { pointerId: 1 });
    expect(useEditorStore.getState().adjustments.curves.luma).toContainEqual({
      x: 100,
      y: 110,
    });
  });

  it("reads tones under view overlays from the recipe", async () => {
    useEditorStore.setState({
      tatTarget: "regionCurve",
      clippingWarnings: true,
    });
    invoke.mockResolvedValue([linearFromSrgb([100, 100, 100])]);
    const el = surface();
    await act(async () => {
      fireEvent.pointerDown(el, {
        button: 0,
        clientX: 10,
        clientY: 100,
        pointerId: 1,
      });
    });
    fireEvent.pointerMove(el, { clientX: 10, clientY: 90, pointerId: 1 });
    fireEvent.pointerUp(el, { pointerId: 1 });
    // The sample renders without view states: the recipe alone is sent.
    expect(invoke.mock.calls[0][1].adjustments.showClipping).not.toBe(true);
    expect(useEditorStore.getState().adjustments.regionCurve!.darks).toBe(5);
    useEditorStore.setState({ clippingWarnings: false });
  });

  const press = async (el: HTMLElement, y = 100) => {
    await act(async () => {
      fireEvent.pointerDown(el, {
        button: 0,
        clientX: 10,
        clientY: y,
        pointerId: 1,
      });
    });
  };

  it("moves the point curve of the channel the curve panel shows", async () => {
    useEditorStore.setState({ tatTarget: "curve", curveChannel: "green" });
    invoke.mockResolvedValue([linearFromSrgb([100, 100, 100])]);
    const el = surface();
    await press(el);
    // The press marks its input level on the curve.
    expect(useEditorStore.getState().tatCurveMarker).toBe(100);
    fireEvent.pointerMove(el, { clientX: 10, clientY: 80, pointerId: 1 });
    fireEvent.pointerUp(el, { pointerId: 1 });
    const { curves } = useEditorStore.getState().adjustments;
    expect(curves.green).toContainEqual({ x: 100, y: 110 });
    expect(curves.luma).toEqual(INITIAL_ADJUSTMENTS.curves.luma);
  });

  it("sets an anchor point on a click, also one that shakes a pixel", async () => {
    useEditorStore.setState({ tatTarget: "curve" });
    invoke.mockResolvedValue([linearFromSrgb([100, 100, 100])]);
    const el = surface();
    await press(el);
    fireEvent.pointerMove(el, { clientX: 10, clientY: 99, pointerId: 1 });
    fireEvent.pointerMove(el, { clientX: 11, clientY: 102, pointerId: 1 });
    fireEvent.pointerUp(el, { pointerId: 1 });
    expect(useEditorStore.getState().adjustments.curves.luma).toEqual([
      { x: 0, y: 0 },
      { x: 100, y: 100 },
      { x: 255, y: 255 },
    ]);
  });

  it("makes one undo step of a whole drag", async () => {
    useEditorStore.setState({ tatTarget: "curve" });
    const before = useEditorStore.getState().adjustments;
    useEditorStore.getState().resetHistory(before);
    invoke.mockResolvedValue([linearFromSrgb([100, 100, 100])]);
    const el = surface();
    await press(el, 200);
    for (const y of [190, 170, 150, 160, 140])
      fireEvent.pointerMove(el, { clientX: 10, clientY: y, pointerId: 1 });
    fireEvent.pointerUp(el, { pointerId: 1 });
    debouncedSetHistory.flush();
    const { history, historyIndex, adjustments } = useEditorStore.getState();
    expect(adjustments.curves.luma).toContainEqual({ x: 100, y: 130 });
    expect(history).toHaveLength(2);
    expect(historyIndex).toBe(1);
    expect(history[0]).toEqual(before);
    expect(history[1].curves.luma).toContainEqual({ x: 100, y: 130 });
  });

  it("marks the input level under the hovering pointer", async () => {
    useEditorStore.setState({ tatTarget: "curve", curveChannel: "red" });
    const lifted = structuredClone(INITIAL_ADJUSTMENTS);
    lifted.curves.luma = [
      { x: 0, y: 0 },
      { x: 100, y: 120 },
      { x: 255, y: 255 },
    ];
    useEditorStore.setState({ adjustments: lifted });
    invoke.mockResolvedValue([linearFromSrgb([100, 100, 100])]);
    const el = surface();
    await act(async () => {
      fireEvent.pointerMove(el, { clientX: 200, clientY: 150, pointerId: 1 });
    });
    expect(invoke).toHaveBeenCalledWith("sample_curve_input", {
      path: "/s/a.NEF",
      normalizedX: 0.5,
      normalizedY: 0.5,
      adjustments: lifted,
    });
    // The red curve's input: the RGB curve's output.
    expect(useEditorStore.getState().tatCurveMarker).toBe(120);
    expect(useEditorStore.getState().adjustments).toBe(lifted);
    fireEvent.pointerLeave(el);
    expect(useEditorStore.getState().tatCurveMarker).toBeNull();
    // A sample still on its way when the pointer leaves sets no marker.
    let resolve!: (value: unknown) => void;
    invoke.mockImplementation(() => new Promise((r) => (resolve = r)));
    fireEvent.pointerMove(el, { clientX: 100, clientY: 100, pointerId: 1 });
    fireEvent.pointerLeave(el);
    await act(async () => resolve([linearFromSrgb([100, 100, 100])]));
    expect(useEditorStore.getState().tatCurveMarker).toBeNull();
    // Other targets do not sample on hover.
    invoke.mockClear();
    useEditorStore.setState({ tatTarget: "regionCurve" });
    fireEvent.pointerMove(el, { clientX: 20, clientY: 20, pointerId: 1 });
    expect(invoke).not.toHaveBeenCalled();
  });
});
