import "../../../i18n";
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
} from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import RelightOverlay from "./RelightOverlay";
import { useEditorStore } from "../../../store/useEditorStore";
import { INITIAL_ADJUSTMENTS } from "../../../utils/adjustments";
import {
  createRelightLight,
  relightDragPatch,
  matchesRelightGeometry,
} from "../../../utils/relight";
import { debouncedSetHistory } from "../../../hooks/useEditorActions";
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
beforeEach(() => {
  vi.stubGlobal("PointerEvent", MouseEvent);
  vi.stubGlobal("requestAnimationFrame", (cb: FrameRequestCallback) => {
    cb(0);
    return 1;
  });
  vi.stubGlobal("cancelAnimationFrame", () => {});
  const adjustments = {
    ...structuredClone(INITIAL_ADJUSTMENTS),
    relightLights: [createRelightLight()],
  };
  useEditorStore.setState({
    ...useEditorStore.getInitialState(),
    selectedImage: { path: "/qa/a.jpg", isReady: true } as never,
    adjustments,
    history: [adjustments],
    historyIndex: 0,
  });
});
afterEach(() => {
  cleanup();
  debouncedSetHistory.cancel();
  vi.unstubAllGlobals();
});
function mount() {
  const view = render(
    <RelightOverlay
      rect={{ left: 0, top: 0, width: 400, height: 200 }}
      zoom={2}
    />,
  );
  view.container.firstElementChild!.getBoundingClientRect = () =>
    ({ left: 50, top: 100, width: 800, height: 400 }) as DOMRect;
  return screen.getByRole("button", { name: "Light 1: move source" });
}
it("moves a light in image coordinates at zoom, commits one undo step, and can undo it", () => {
  const handle = mount();
  fireEvent.pointerDown(handle, { button: 0, clientX: 210, clientY: 180 });
  fireEvent.pointerMove(handle, { clientX: 290, clientY: 220 });
  expect(useEditorStore.getState().isSliderDragging).toBe(true);
  fireEvent.pointerUp(handle, { clientX: 290, clientY: 220 });
  const state = useEditorStore.getState();
  expect(state.adjustments.relightLights![0].sourceX).toBeCloseTo(30);
  expect(state.adjustments.relightLights![0].sourceY).toBeCloseTo(30);
  expect(state.history).toHaveLength(2);
  expect(state.isSliderDragging).toBe(false);
  act(() => state.undo());
  expect(useEditorStore.getState().adjustments.relightLights![0].sourceX).toBe(
    20,
  );
});
it("Escape restores the light; a click only selects and does not add history", () => {
  const handle = mount();
  fireEvent.pointerDown(handle, { button: 0, clientX: 210, clientY: 180 });
  fireEvent.pointerMove(handle, { clientX: 290, clientY: 220 });
  fireEvent.keyDown(handle, { key: "Escape" });
  expect(useEditorStore.getState().adjustments.relightLights![0].sourceX).toBe(
    20,
  );
  expect(useEditorStore.getState().history).toHaveLength(1);
  fireEvent.pointerDown(handle, { button: 0, clientX: 210, clientY: 180 });
  fireEvent.pointerUp(handle, { clientX: 210, clientY: 180 });
  expect(useEditorStore.getState().history).toHaveLength(1);
});
it("never writes a late pointer event into another photo and releases interaction on unmount", () => {
  const handle = mount();
  fireEvent.pointerDown(handle, { button: 0, clientX: 210, clientY: 180 });
  act(() =>
    useEditorStore.setState({
      selectedImage: { path: "/qa/b.jpg", isReady: true } as never,
    }),
  );
  fireEvent.pointerUp(handle, { clientX: 290, clientY: 220 });
  expect(useEditorStore.getState().adjustments.relightLights![0].sourceX).toBe(
    20,
  );
  expect(useEditorStore.getState().isSliderDragging).toBe(false);
  fireEvent.keyDown(handle, { key: "ArrowRight", shiftKey: true });
  expect(useEditorStore.getState().adjustments.relightLights![0].sourceX).toBe(
    25,
  );
});
it("moves the pattern independently and rotates/scales using isotropic photo coordinates", () => {
  const l = createRelightLight();
  expect(
    relightDragPatch(l, "move", { x: 0, y: 0 }, { x: 10, y: -5 }, 400, 200),
  ).toMatchObject({ x: 60, y: 45, sourceX: 20, sourceY: 20 });
  expect(
    relightDragPatch(l, "rotate", { x: 75, y: 50 }, { x: 50, y: 100 }, 400, 200)
      .patternRotation,
  ).toBeCloseTo(90);
  expect(
    relightDragPatch(l, "scale", { x: 75, y: 50 }, { x: 100, y: 50 }, 400, 200)
      .scale,
  ).toBe(100);
});

it("rejects legacy depth after crop or straightening and accepts matching displayed geometry", () => {
  expect(matchesRelightGeometry({ rotation: 0, crop: null }, {})).toBe(true);
  expect(matchesRelightGeometry({ rotation: 10 }, {})).toBe(false);
  const crop = { x: 0.1, y: 0.2, width: 0.5, height: 0.6 };
  expect(matchesRelightGeometry({ crop }, {})).toBe(false);
  expect(
    matchesRelightGeometry({ crop, rotation: 10 }, { rotation: 10, crop }),
  ).toBe(true);
  expect(
    matchesRelightGeometry({ crop: null, rotation: 0 }, { rotation: 10, crop }),
  ).toBe(false);
});

it("Escape also restores absent source and pattern controls in legacy recipes", () => {
  const legacy = { ...createRelightLight() };
  delete legacy.sourceX;
  delete legacy.sourceY;
  delete legacy.patternRotation;
  useEditorStore.setState((s) => ({
    adjustments: { ...s.adjustments, relightLights: [legacy] },
  }));
  const handle = mount();
  fireEvent.pointerDown(handle, { button: 0, clientX: 210, clientY: 180 });
  fireEvent.pointerMove(handle, { clientX: 290, clientY: 220 });
  fireEvent.keyDown(handle, { key: "Escape" });
  expect(useEditorStore.getState().adjustments.relightLights![0]).toEqual(
    legacy,
  );
});

it("returning to the starting point before release restores the original without a history step", () => {
  const handle = mount();
  const original = useEditorStore.getState().adjustments.relightLights![0];
  fireEvent.pointerDown(handle, { button: 0, clientX: 210, clientY: 180 });
  fireEvent.pointerMove(handle, { clientX: 290, clientY: 220 });
  fireEvent.pointerUp(handle, { clientX: 210, clientY: 180 });
  expect(useEditorStore.getState().adjustments.relightLights![0]).toEqual(
    original,
  );
  expect(useEditorStore.getState().history).toHaveLength(1);
});
