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
import { useEditorStore } from "../../../store/useEditorStore";
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
    invoke.mockResolvedValue([100, 100, 100]);
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
    invoke.mockResolvedValue([100, 100, 100]);
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
});
