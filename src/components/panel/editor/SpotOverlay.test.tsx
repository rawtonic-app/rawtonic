import "../../../i18n";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

const { invoke, listeners } = vi.hoisted(() => ({
  invoke: vi.fn(),
  listeners: new Map<string, (event: { payload: unknown }) => void>(),
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
vi.mock("@tauri-apps/api/event", () => ({
  listen: (name: string, handler: (event: { payload: unknown }) => void) => {
    listeners.set(name, handler);
    return Promise.resolve(() => listeners.delete(name));
  },
}));

import SpotOverlay from "./SpotOverlay";
import { useEditorStore } from "../../../store/useEditorStore";
import { INITIAL_ADJUSTMENTS } from "../../../utils/adjustments";

beforeAll(() => {
  Element.prototype.setPointerCapture = () => {};
  Element.prototype.hasPointerCapture = () => false;
  Element.prototype.releasePointerCapture = () => {};
});

beforeEach(() => {
  useEditorStore.setState({
    selectedImage: { path: "/s/a.NEF", isReady: true, isRaw: true } as never,
    adjustments: structuredClone(INITIAL_ADJUSTMENTS),
    isSpotToolActive: true,
    activeSpotId: null,
    spotSettings: { mode: "heal", size: 20, feather: 50, opacity: 100 },
    spotVisualize: { on: false, sensitivity: 50 },
  });
  invoke.mockReset();
});
afterEach(cleanup);

// The box shows mask pixels 100…500 × 50…350 at half size (crop at 100, 50).
const overlay = () => {
  render(
    <SpotOverlay rect={{ left: 0, top: 0, width: 200, height: 150 }} cropX={100} cropY={50} scale={0.5} zoom={1} />,
  );
  const el = screen.getByTestId("spot-overlay");
  el.getBoundingClientRect = () => ({ left: 0, top: 0, width: 200, height: 150 }) as DOMRect;
  return el;
};

describe("spot removal overlay", () => {
  it("places a spot with the backend's automatic source", async () => {
    invoke.mockResolvedValue([260, 150]);
    const el = overlay();
    await act(async () => {
      fireEvent.pointerDown(el, { button: 0, clientX: 50, clientY: 50, pointerId: 1 });
      fireEvent.pointerUp(el, { clientX: 50, clientY: 50, pointerId: 1 });
    });
    expect(invoke).toHaveBeenCalledWith(
      "find_spot_source",
      expect.objectContaining({ x: 200, y: 150, radius: 20, points: null }),
    );
    const { adjustments, activeSpotId } = useEditorStore.getState();
    expect(adjustments.spots).toHaveLength(1);
    expect(adjustments.spots[0]).toMatchObject({
      mode: "heal",
      x: 200,
      y: 150,
      radius: 20,
      sourceX: 260,
      sourceY: 150,
      feather: 50,
      opacity: 100,
      visible: true,
    });
    expect(activeSpotId).toBe(adjustments.spots[0].id);
  });

  it("moves the selected spot's source on release and deletes the spot with Delete", async () => {
    useEditorStore.setState({
      adjustments: {
        ...structuredClone(INITIAL_ADJUSTMENTS),
        spots: [
          {
            id: "s1",
            mode: "clone",
            x: 200,
            y: 150,
            radius: 20,
            sourceX: 260,
            sourceY: 150,
            feather: 50,
            opacity: 100,
            visible: true,
          },
        ],
      },
      activeSpotId: "s1",
    });
    const el = overlay();
    // Press inside the source circle (box 80, 50) and drag 10 box px down.
    fireEvent.pointerDown(el, { button: 0, clientX: 80, clientY: 50, pointerId: 1 });
    fireEvent.pointerMove(el, { clientX: 80, clientY: 60, pointerId: 1 });
    // Not committed while dragging.
    expect(useEditorStore.getState().adjustments.spots[0].sourceY).toBe(150);
    fireEvent.pointerUp(el, { clientX: 80, clientY: 60, pointerId: 1 });
    expect(useEditorStore.getState().adjustments.spots[0]).toMatchObject({ sourceX: 260, sourceY: 170 });
    expect(invoke).not.toHaveBeenCalled();
    // The rim resizes.
    // The rim's top (mask 200, 130) is at box (50, 40); 5 box px up = 10 px.
    fireEvent.pointerDown(el, { button: 0, clientX: 50, clientY: 40, pointerId: 1 });
    fireEvent.pointerMove(el, { clientX: 50, clientY: 35, pointerId: 1 });
    fireEvent.pointerUp(el, { clientX: 50, clientY: 35, pointerId: 1 });
    expect(useEditorStore.getState().adjustments.spots[0].radius).toBeCloseTo(30);
    fireEvent.keyDown(window, { key: "Delete" });
    expect(useEditorStore.getState().adjustments.spots).toHaveLength(0);
    expect(useEditorStore.getState().activeSpotId).toBeNull();
  });

  it("paints a heal stroke when dragged, with one automatic source offset", async () => {
    invoke.mockResolvedValue([200, 210]);
    const el = overlay();
    // Box (50, 50) → (90, 50): mask (200, 150) → (280, 150), a point every 4 px.
    await act(async () => {
      fireEvent.pointerDown(el, { button: 0, clientX: 50, clientY: 50, pointerId: 1 });
      for (let x = 52; x <= 90; x += 2) fireEvent.pointerMove(el, { clientX: x, clientY: 50, pointerId: 1 });
      fireEvent.pointerUp(el, { clientX: 90, clientY: 50, pointerId: 1 });
    });
    const call = invoke.mock.calls.find(([name]) => name === "find_spot_source")!;
    const points = (call[1] as { points: { x: number; y: number }[] }).points;
    expect(points[0]).toEqual({ x: 200, y: 150 });
    expect(points.at(-1)).toEqual({ x: 280, y: 150 });
    const [spot] = useEditorStore.getState().adjustments.spots;
    expect(spot).toMatchObject({ x: 200, y: 150, radius: 20, sourceX: 200, sourceY: 210 });
    expect(spot.points).toEqual(points);
  });

  it("moves a stroke with its source and hit-tests along its path", () => {
    const points = [
      { x: 200, y: 150 },
      { x: 300, y: 150 },
    ];
    useEditorStore.setState({
      adjustments: {
        ...structuredClone(INITIAL_ADJUSTMENTS),
        spots: [
          {
            id: "s1",
            mode: "heal",
            x: 200,
            y: 150,
            radius: 10,
            sourceX: 200,
            sourceY: 200,
            feather: 0,
            opacity: 100,
            visible: true,
            points,
          },
        ],
      },
      activeSpotId: null,
    });
    const el = overlay();
    // Mask (260, 150) is on the stroke, box (80, 50); drag 10 box px right.
    fireEvent.pointerDown(el, { button: 0, clientX: 80, clientY: 50, pointerId: 1 });
    fireEvent.pointerMove(el, { clientX: 90, clientY: 50, pointerId: 1 });
    fireEvent.pointerUp(el, { clientX: 90, clientY: 50, pointerId: 1 });
    expect(useEditorStore.getState().adjustments.spots[0]).toMatchObject({
      x: 220,
      sourceX: 220,
      sourceY: 200,
      points: [
        { x: 220, y: 150 },
        { x: 320, y: 150 },
      ],
    });
    expect(invoke).not.toHaveBeenCalled();
  });

  it("removes an area with the local inpainting model as an AI patch, and deletes it again", async () => {
    const patchData = { color: "c", mask: "m", offsetX: 0, offsetY: 0, width: 4, height: 4, isSrgbEncoded: true };
    invoke.mockResolvedValue(JSON.stringify(patchData));
    useEditorStore.setState({ spotSettings: { mode: "remove", size: 20, feather: 0, opacity: 100 } });
    const el = overlay();
    await act(async () => {
      fireEvent.pointerDown(el, { button: 0, clientX: 50, clientY: 50, pointerId: 1 });
      fireEvent.pointerUp(el, { clientX: 50, clientY: 50, pointerId: 1 });
    });
    expect(invoke).toHaveBeenCalledWith(
      "invoke_generative_replace_with_mask_def",
      expect.objectContaining({ path: "/s/a.NEF", useFastInpaint: true }),
    );
    const [patch] = useEditorStore.getState().adjustments.aiPatches;
    expect(patch).toMatchObject({ name: "Remove", isLoading: false, patchData });
    expect(patch.subMasks[0].parameters.lines).toEqual([
      expect.objectContaining({ points: [{ x: 200, y: 150 }], brushSize: 40 }),
    ]);
    // No heal or clone spot is made.
    expect(useEditorStore.getState().adjustments.spots).toHaveLength(0);
    expect(useEditorStore.getState().activeSpotId).toBe(patch.id);
    fireEvent.keyDown(window, { key: "Delete" });
    expect(useEditorStore.getState().adjustments.aiPatches).toHaveLength(0);
  });

  it("shows a slow removal's download and steps, and Cancel stops it", async () => {
    let failFill: (error: unknown) => void = () => {};
    invoke.mockImplementation((command: string) =>
      command === "invoke_generative_replace_with_mask_def"
        ? new Promise((_, reject) => {
            failFill = reject;
          })
        : Promise.resolve(),
    );
    useEditorStore.setState({ spotSettings: { mode: "remove", size: 20, feather: 0, opacity: 100 } });
    const el = overlay();
    await act(async () => {
      fireEvent.pointerDown(el, { button: 0, clientX: 50, clientY: 50, pointerId: 1 });
      fireEvent.pointerUp(el, { clientX: 50, clientY: 50, pointerId: 1 });
    });
    const fill = invoke.mock.calls.find(([name]) => name === "invoke_generative_replace_with_mask_def")!;
    const { taskId } = fill[1] as { taskId: string };
    expect(screen.queryByTestId("remove-progress")).toBeNull();
    act(() => listeners.get("ai-model-download-progress")!({ payload: { received: 50, total: 200 } }));
    expect(screen.getByTestId("remove-progress").textContent).toContain("25");
    // Another task's steps are not shown.
    act(() => listeners.get("ai-fill-progress")!({ payload: { taskId: "other", done: 1, total: 9 } }));
    expect(screen.getByTestId("remove-progress").textContent).toContain("25");
    act(() => listeners.get("ai-fill-progress")!({ payload: { taskId, done: 3, total: 9 } }));
    expect(screen.getByTestId("remove-progress").textContent).toMatch(/3\D+9/);
    fireEvent.click(screen.getByRole("button"));
    expect(invoke).toHaveBeenCalledWith("cancel_ai_task", { taskId });
    await act(async () => failFill("Task cancelled"));
    expect(screen.queryByTestId("remove-progress")).toBeNull();
    expect(useEditorStore.getState().adjustments.aiPatches ?? []).toHaveLength(0);
  });

  it("removes an object selected by a click: object mask, then fill, as one AI patch", async () => {
    const patchData = { color: "c", mask: "m", offsetX: 0, offsetY: 0, width: 4, height: 4, isSrgbEncoded: true };
    let releaseMask: (value: unknown) => void = () => {};
    invoke.mockImplementation((command: string) => {
      if (command === "generate_ai_subject_mask")
        return new Promise((resolve) => {
          releaseMask = resolve;
        });
      return Promise.resolve(JSON.stringify(patchData));
    });
    useEditorStore.setState({
      spotSettings: { mode: "remove", size: 20, feather: 0, opacity: 100, detectObjects: true },
    });
    const el = overlay();
    await act(async () => {
      fireEvent.pointerDown(el, { button: 0, clientX: 50, clientY: 50, pointerId: 1 });
      fireEvent.pointerMove(el, { clientX: 51, clientY: 51, pointerId: 1 });
      fireEvent.pointerUp(el, { clientX: 51, clientY: 51, pointerId: 1 });
    });
    // A press that barely moves is a click: a point prompt at mask (200, 150).
    expect(invoke).toHaveBeenCalledWith(
      "generate_ai_subject_mask",
      expect.objectContaining({ path: "/s/a.NEF", startPoint: [200, 150], endPoint: [200, 150] }),
    );
    // Nothing reaches the recipe while the object is selected and filled.
    expect(useEditorStore.getState().adjustments.aiPatches).toHaveLength(0);
    expect(screen.getByTestId("remove-object-pending")).toBeTruthy();
    await act(async () => {
      releaseMask({ maskDataBase64: "data:image/png;base64,AA", rotation: 0, orientationSteps: 0 });
    });
    const fill = invoke.mock.calls.find(([name]) => name === "invoke_generative_replace_with_mask_def")!;
    const definition = (fill[1] as { patchDefinition: { subMasks: { type: string; parameters: object }[] } })
      .patchDefinition;
    expect(fill[1]).toMatchObject({ useFastInpaint: true });
    expect(definition.subMasks).toHaveLength(1);
    expect(definition.subMasks[0].type).toBe("quick-eraser");
    // The selection is grown and feathered, so the fill takes the object's edge along.
    expect(definition.subMasks[0].parameters).toMatchObject({
      maskDataBase64: "data:image/png;base64,AA",
      startX: 200,
      startY: 150,
      endX: 200,
      endY: 150,
      grow: 50,
      feather: 50,
    });
    const [patch] = useEditorStore.getState().adjustments.aiPatches;
    expect(patch).toMatchObject({ name: "Remove", isLoading: false, patchData, visible: true });
    expect(useEditorStore.getState().adjustments.spots).toHaveLength(0);
    expect(useEditorStore.getState().activeSpotId).toBe(patch.id);
    expect(screen.getByTestId("remove-object")).toBeTruthy();
    expect(screen.queryByTestId("remove-object-pending")).toBeNull();
    // Escape deselects; a click on its pin selects it again, Delete removes it.
    fireEvent.keyDown(window, { key: "Escape" });
    expect(useEditorStore.getState().activeSpotId).toBeNull();
    fireEvent.pointerDown(el, { button: 0, clientX: 50, clientY: 50, pointerId: 1 });
    fireEvent.pointerUp(el, { clientX: 50, clientY: 50, pointerId: 1 });
    expect(useEditorStore.getState().activeSpotId).toBe(patch.id);
    expect(invoke.mock.calls.filter(([name]) => name === "generate_ai_subject_mask")).toHaveLength(1);
    fireEvent.keyDown(window, { key: "Delete" });
    expect(useEditorStore.getState().adjustments.aiPatches).toHaveLength(0);
  });

  it("removes an object inside a box dragged in any direction", async () => {
    invoke.mockImplementation((command: string) =>
      Promise.resolve(command === "generate_ai_subject_mask" ? { maskDataBase64: "m" } : JSON.stringify({ color: "c" })),
    );
    useEditorStore.setState({
      spotSettings: { mode: "remove", size: 20, feather: 0, opacity: 100, detectObjects: true, objectSelect: "box" },
    });
    const el = overlay();
    // Box (90, 70) → (50, 40): mask (280, 190) → (200, 130).
    await act(async () => {
      fireEvent.pointerDown(el, { button: 0, clientX: 90, clientY: 70, pointerId: 1 });
      fireEvent.pointerMove(el, { clientX: 70, clientY: 55, pointerId: 1 });
      fireEvent.pointerMove(el, { clientX: 50, clientY: 40, pointerId: 1 });
      fireEvent.pointerUp(el, { clientX: 50, clientY: 40, pointerId: 1 });
    });
    expect(invoke).toHaveBeenCalledWith(
      "generate_ai_subject_mask",
      expect.objectContaining({ startPoint: [200, 130], endPoint: [280, 190] }),
    );
    const [patch] = useEditorStore.getState().adjustments.aiPatches;
    expect(patch.subMasks[0].parameters).toMatchObject({ startX: 200, startY: 130, endX: 280, endY: 190 });
  });

  it("selects an object brushed over loosely by the box around the stroke", async () => {
    invoke.mockImplementation((command: string) =>
      Promise.resolve(command === "generate_ai_subject_mask" ? { maskDataBase64: "m" } : JSON.stringify({ color: "c" })),
    );
    useEditorStore.setState({
      spotSettings: { mode: "remove", size: 20, feather: 0, opacity: 100, detectObjects: true },
    });
    const el = overlay();
    // Box (50, 50) → (90, 70): mask (200, 150) → (280, 190), brush radius 20.
    await act(async () => {
      fireEvent.pointerDown(el, { button: 0, clientX: 50, clientY: 50, pointerId: 1 });
      for (let i = 1; i <= 20; i++)
        fireEvent.pointerMove(el, { clientX: 50 + 2 * i, clientY: 50 + i, pointerId: 1 });
    });
    expect(screen.getByTestId("remove-object-stroke")).toBeTruthy();
    await act(async () => {
      fireEvent.pointerUp(el, { clientX: 90, clientY: 70, pointerId: 1 });
    });
    expect(invoke).toHaveBeenCalledWith(
      "generate_ai_subject_mask",
      expect.objectContaining({ startPoint: [180, 130], endPoint: [300, 210] }),
    );
    const [patch] = useEditorStore.getState().adjustments.aiPatches;
    expect(patch.subMasks).toHaveLength(1);
    expect(patch.subMasks[0]).toMatchObject({ type: "quick-eraser", mode: "additive" });
    expect(screen.queryByTestId("remove-object-stroke")).toBeNull();
  });

  it("adds to the selected object removal with Shift and subtracts with Alt, refilling it in one step", async () => {
    let n = 0;
    invoke.mockImplementation((command: string) =>
      Promise.resolve(
        command === "generate_ai_subject_mask" ? { maskDataBase64: `m${n++}` } : JSON.stringify({ color: `c${n}` }),
      ),
    );
    useEditorStore.setState({
      spotSettings: { mode: "remove", size: 20, feather: 0, opacity: 100, detectObjects: true },
    });
    const el = overlay();
    const click = async (x: number, y: number, mods: { shiftKey?: boolean; altKey?: boolean } = {}) => {
      await act(async () => {
        fireEvent.pointerDown(el, { button: 0, clientX: x, clientY: y, pointerId: 1, ...mods });
        fireEvent.pointerUp(el, { clientX: x, clientY: y, pointerId: 1, ...mods });
      });
    };
    await click(50, 50);
    const [first] = useEditorStore.getState().adjustments.aiPatches;
    expect(useEditorStore.getState().activeSpotId).toBe(first.id);
    // Shift-click elsewhere (even on the pin): the same removal, one more component.
    await click(80, 60, { shiftKey: true });
    let patches = useEditorStore.getState().adjustments.aiPatches;
    expect(patches).toHaveLength(1);
    // A new id in the old one's place (the preview sends pixels once per id).
    expect(patches[0].id).not.toBe(first.id);
    expect(useEditorStore.getState().activeSpotId).toBe(patches[0].id);
    expect(patches[0].subMasks.map((m: { mode: string }) => m.mode)).toEqual(["additive", "additive"]);
    expect(patches[0].patchData).toEqual({ color: "c2" });
    // The refill gets the whole selection and the photo without the old removal.
    const fills = invoke.mock.calls.filter(([name]) => name === "invoke_generative_replace_with_mask_def");
    const refill = fills[1][1] as { patchDefinition: { subMasks: unknown[] }; currentAdjustments: { aiPatches: unknown[] } };
    expect(refill.patchDefinition.subMasks).toHaveLength(2);
    expect(refill.currentAdjustments.aiPatches).toHaveLength(0);
    await click(50, 50, { altKey: true });
    patches = useEditorStore.getState().adjustments.aiPatches;
    expect(patches).toHaveLength(1);
    expect(patches[0].subMasks.map((m: { mode: string }) => m.mode)).toEqual(["additive", "additive", "subtractive"]);
    expect(patches[0].subMasks[2].parameters).toMatchObject({ maskDataBase64: "m2", startX: 200, startY: 150 });
    // Without a selected removal, Shift starts a new one.
    act(() => useEditorStore.setState({ activeSpotId: null }));
    await click(60, 60, { shiftKey: true });
    expect(useEditorStore.getState().adjustments.aiPatches).toHaveLength(2);
  });

  it("leaves the recipe unchanged when the object cannot be selected", async () => {
    invoke.mockRejectedValue("AI model not installed");
    useEditorStore.setState({
      spotSettings: { mode: "remove", size: 20, feather: 0, opacity: 100, detectObjects: true },
    });
    const el = overlay();
    await act(async () => {
      fireEvent.pointerDown(el, { button: 0, clientX: 50, clientY: 50, pointerId: 1 });
      fireEvent.pointerUp(el, { clientX: 50, clientY: 50, pointerId: 1 });
    });
    expect(invoke).not.toHaveBeenCalledWith("invoke_generative_replace_with_mask_def", expect.anything());
    expect(useEditorStore.getState().adjustments.aiPatches).toHaveLength(0);
    expect(screen.queryByTestId("remove-object-pending")).toBeNull();
  });

  it("shows the backend's Visualize Spots view when it is switched on", async () => {
    vi.useFakeTimers();
    URL.createObjectURL = vi.fn(() => "blob:view");
    URL.revokeObjectURL = vi.fn();
    invoke.mockResolvedValue(new ArrayBuffer(4));
    useEditorStore.setState({ spotVisualize: { on: true, sensitivity: 70 } });
    overlay();
    await act(async () => {
      vi.advanceTimersByTime(200);
    });
    vi.useRealTimers();
    expect(invoke).toHaveBeenCalledWith(
      "visualize_spots",
      expect.objectContaining({ sensitivity: 70, maxEdge: 2400 }),
    );
    expect(screen.getByTestId("spot-overlay").querySelector("img")?.getAttribute("src")).toBe("blob:view");
  });
});
