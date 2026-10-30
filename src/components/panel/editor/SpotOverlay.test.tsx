import "../../../i18n";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

const { invoke, listeners } = vi.hoisted(() => ({
  invoke: vi.fn(),
  listeners: new Map<string, (event: { payload: unknown }) => void>(),
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
const { toastWarn, toastInfo } = vi.hoisted(() => ({ toastWarn: vi.fn(), toastInfo: vi.fn() }));
vi.mock("../../../activity/notify", () => ({ notify: Object.assign(vi.fn(), { warn: toastWarn, info: toastInfo, error: vi.fn() }) }));
vi.mock("@tauri-apps/api/event", () => ({
  listen: (name: string, handler: (event: { payload: unknown }) => void) => {
    listeners.set(name, handler);
    return Promise.resolve(() => listeners.delete(name));
  },
}));

import SpotOverlay, { pathToBox } from "./SpotOverlay";
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

  // A heal circle of radius 20 at mask (x, y): box ((x − 100) / 2, (y − 50) / 2).
  const circle = (id: string, x: number, y: number, visible = true) => ({
    id,
    mode: "heal" as const,
    x,
    y,
    radius: 20,
    sourceX: x + 60,
    sourceY: y,
    feather: 50,
    opacity: 100,
    visible,
  });
  // A removal painted as a circle of radius 20 at mask (x, y).
  const removal = (id: string, x: number, y: number) => ({
    id,
    name: "Remove",
    isLoading: false,
    invert: false,
    visible: true,
    prompt: "",
    patchData: null,
    subMasks: [
      {
        id: `${id}-m`,
        type: "brush",
        mode: "additive",
        invert: false,
        opacity: 100,
        visible: true,
        parameters: { lines: [{ points: [{ x, y }], brushSize: 40, feather: 0.2, tool: "brush" }] },
      },
    ],
  });

  it("deletes the spot or removal under an Alt-click (Lightroom) and places nothing", () => {
    useEditorStore.setState({
      adjustments: {
        ...structuredClone(INITIAL_ADJUSTMENTS),
        spots: [circle("s1", 200, 150), circle("s2", 400, 250)],
        aiPatches: [removal("r1", 300, 300)] as never,
      },
      activeSpotId: "s1",
    });
    const el = overlay();
    fireEvent.pointerDown(el, { button: 0, altKey: true, clientX: 50, clientY: 50, pointerId: 1 });
    fireEvent.pointerUp(el, { clientX: 50, clientY: 50, pointerId: 1 });
    expect(useEditorStore.getState().adjustments.spots.map((s) => s.id)).toEqual(["s2"]);
    expect(useEditorStore.getState().activeSpotId).toBeNull();
    // The removal at mask (300, 300) is at box (100, 125).
    fireEvent.pointerDown(el, { button: 0, altKey: true, clientX: 100, clientY: 125, pointerId: 1 });
    fireEvent.pointerUp(el, { clientX: 100, clientY: 125, pointerId: 1 });
    expect(useEditorStore.getState().adjustments.aiPatches).toHaveLength(0);
    // An Alt-click on the empty photo neither places nor deletes.
    fireEvent.pointerDown(el, { button: 0, altKey: true, clientX: 10, clientY: 140, pointerId: 1 });
    fireEvent.pointerUp(el, { clientX: 10, clientY: 140, pointerId: 1 });
    expect(useEditorStore.getState().adjustments.spots.map((s) => s.id)).toEqual(["s2"]);
    expect(invoke).not.toHaveBeenCalled();
  });

  it("deletes every spot and removal an Alt-drag box reaches, in one change", () => {
    useEditorStore.setState({
      adjustments: {
        ...structuredClone(INITIAL_ADJUSTMENTS),
        spots: [circle("s1", 200, 150), circle("s2", 400, 250), circle("s3", 480, 330)],
        aiPatches: [removal("r1", 300, 200)] as never,
      },
    });
    const el = overlay();
    const changes = vi.fn();
    const stop = useEditorStore.subscribe((s, before) => s.adjustments !== before.adjustments && changes());
    // From box (30, 30) to (160, 110): mask 160…420 × 110…270.
    fireEvent.pointerDown(el, { button: 0, altKey: true, clientX: 30, clientY: 30, pointerId: 1 });
    fireEvent.pointerMove(el, { clientX: 160, clientY: 110, pointerId: 1 });
    expect(screen.getByTestId("spot-erase-box")).toBeTruthy();
    fireEvent.pointerUp(el, { clientX: 160, clientY: 110, pointerId: 1 });
    stop();
    expect(useEditorStore.getState().adjustments.spots.map((s) => s.id)).toEqual(["s3"]);
    expect(useEditorStore.getState().adjustments.aiPatches).toHaveLength(0);
    expect(changes).toHaveBeenCalledTimes(1);
    expect(screen.queryByTestId("spot-erase-box")).toBeNull();
  });

  it("deletes by an Alt-drag that starts on a spot, and strokes crossing the box between their points", () => {
    useEditorStore.setState({
      adjustments: {
        ...structuredClone(INITIAL_ADJUSTMENTS),
        spots: [
          circle("s1", 200, 150),
          circle("s2", 400, 150),
          // A stroke with two points only, crossing the box between them.
          { ...circle("st", 150, 300), points: [{ x: 150, y: 300 }, { x: 480, y: 300 }] },
          circle("far", 480, 120),
        ],
        aiPatches: [{ ...removal("r1", 120, 250), subMasks: [] }] as never,
      },
    });
    const el = overlay();
    // From s1's centre (box 50, 50) to box (150, 140): mask 200…400 × 150…330.
    fireEvent.pointerDown(el, { button: 0, altKey: true, clientX: 50, clientY: 50, pointerId: 1 });
    fireEvent.pointerMove(el, { clientX: 150, clientY: 140, pointerId: 1 });
    fireEvent.pointerUp(el, { clientX: 150, clientY: 140, pointerId: 1 });
    expect(useEditorStore.getState().adjustments.spots.map((s) => s.id)).toEqual(["far"]);
  });

  it("deletes nothing when an Alt-click or Alt-drag is cancelled", () => {
    useEditorStore.setState({
      adjustments: {
        ...structuredClone(INITIAL_ADJUSTMENTS),
        spots: [circle("s1", 200, 150), circle("s2", 400, 250)],
      },
    });
    const el = overlay();
    fireEvent.pointerDown(el, { button: 0, altKey: true, clientX: 50, clientY: 50, pointerId: 1 });
    fireEvent.pointerCancel(el, { clientX: 50, clientY: 50, pointerId: 1 });
    fireEvent.pointerDown(el, { button: 0, altKey: true, clientX: 30, clientY: 30, pointerId: 1 });
    fireEvent.pointerMove(el, { clientX: 160, clientY: 110, pointerId: 1 });
    fireEvent.pointerCancel(el, { clientX: 160, clientY: 110, pointerId: 1 });
    expect(useEditorStore.getState().adjustments.spots.map((s) => s.id)).toEqual(["s1", "s2"]);
    expect(screen.queryByTestId("spot-erase-box")).toBeNull();
  });

  it("measures a path's distance to a box, zero where a segment crosses it", () => {
    const lo = { x: 250, y: 150 };
    const hi = { x: 350, y: 250 };
    expect(pathToBox([{ x: 100, y: 200 }, { x: 500, y: 200 }], lo, hi)).toBe(0);
    expect(pathToBox([{ x: 300, y: 200 }], lo, hi)).toBe(0);
    expect(pathToBox([{ x: 100, y: 300 }, { x: 500, y: 300 }], lo, hi)).toBeCloseTo(50);
    expect(pathToBox([{ x: 360, y: 100 }], lo, hi)).toBeCloseTo(Math.hypot(10, 50));
    // A segment passing a corner diagonally.
    expect(pathToBox([{ x: 340, y: 270 }, { x: 370, y: 240 }], lo, hi)).toBeCloseTo(10 / Math.SQRT2);
  });

  it("selects the spot under a right-click for the settings popover, placing nothing", () => {
    useEditorStore.setState({
      adjustments: { ...structuredClone(INITIAL_ADJUSTMENTS), spots: [circle("s1", 200, 150)] },
    });
    const el = overlay();
    fireEvent.pointerDown(el, { button: 2, clientX: 50, clientY: 50, pointerId: 1 });
    fireEvent.pointerUp(el, { button: 2, clientX: 50, clientY: 50, pointerId: 1 });
    expect(useEditorStore.getState().activeSpotId).toBe("s1");
    expect(useEditorStore.getState().adjustments.spots).toHaveLength(1);
    expect(invoke).not.toHaveBeenCalled();
  });

  it("draws a hidden spot faintly, still there to select", () => {
    useEditorStore.setState({
      adjustments: {
        ...structuredClone(INITIAL_ADJUSTMENTS),
        spots: [circle("s1", 200, 150, false), circle("s2", 400, 250)],
      },
    });
    const el = overlay();
    const groups = [...el.querySelectorAll("svg > g")];
    expect(groups.map((g) => g.getAttribute("opacity"))).toEqual(["0.4", null]);
    fireEvent.pointerDown(el, { button: 0, clientX: 50, clientY: 50, pointerId: 1 });
    fireEvent.pointerUp(el, { clientX: 50, clientY: 50, pointerId: 1 });
    expect(useEditorStore.getState().activeSpotId).toBe("s1");
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
      expect.objectContaining({ patchDefinition: expect.anything() }),
    );
    // Inpainting is always local: no backend choice reaches the command.
    expect(invoke.mock.calls.find(([name]) => name === "invoke_generative_replace_with_mask_def")![1]).not.toHaveProperty(
      "useFastInpaint",
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
    act(() => listeners.get("ai-fill-progress")!({ payload: { taskId, phase: "preparing", done: 0, total: 0 } }));
    expect(screen.getByTestId("remove-progress").textContent).toContain("Preparing large-area removal");
    act(() => listeners.get("ai-model-download-progress")!({ payload: { received: 50, total: 200 } }));
    expect(screen.getByTestId("remove-progress").textContent).toContain("25");
    // Even a task queued behind the loader returns to preparation at 100%.
    act(() => listeners.get("ai-model-download-progress")!({ payload: { received: 200, total: 200 } }));
    expect(screen.getByTestId("remove-progress").textContent).toContain("Preparing large-area removal");
    // The GPU check follows the last download.
    act(() => listeners.get("ai-fill-progress")!({ payload: { taskId, phase: "preparing", done: 0, total: 0 } }));
    expect(screen.getByTestId("remove-progress").textContent).toContain("Preparing large-area removal");
    // Another task's steps are not shown.
    act(() => listeners.get("ai-fill-progress")!({ payload: { taskId: "other", done: 1, total: 9 } }));
    expect(screen.getByTestId("remove-progress").textContent).toContain("Preparing large-area removal");
    act(() => listeners.get("ai-fill-progress")!({ payload: { taskId, done: 3, total: 9 } }));
    expect(screen.getByTestId("remove-progress").textContent).toMatch(/3\D+9/);
    fireEvent.click(screen.getByRole("button"));
    expect(invoke).toHaveBeenCalledWith("cancel_ai_task", { taskId });
    await act(async () => failFill("Task cancelled"));
    expect(screen.queryByTestId("remove-progress")).toBeNull();
    expect(useEditorStore.getState().adjustments.aiPatches ?? []).toHaveLength(0);
  });

  it("shows one percent over a model of several files and says when the smaller model filled", async () => {
    invoke.mockImplementation((command: string) =>
      command === "invoke_generative_replace_with_mask_def" ? new Promise(() => {}) : Promise.resolve(),
    );
    useEditorStore.setState({ spotSettings: { mode: "remove", size: 20, feather: 0, opacity: 100 } });
    const el = overlay();
    await act(async () => {
      fireEvent.pointerDown(el, { button: 0, clientX: 50, clientY: 50, pointerId: 1 });
      fireEvent.pointerUp(el, { clientX: 50, clientY: 50, pointerId: 1 });
    });
    const fill = invoke.mock.calls.find(([name]) => name === "invoke_generative_replace_with_mask_def")!;
    const { taskId } = fill[1] as { taskId: string };
    // The second file's first half: 60 % of the whole, not 50 % of the file.
    act(() =>
      listeners.get("ai-model-download-progress")!({
        payload: { received: 100, total: 200, batchReceived: 600, batchTotal: 1000 },
      }),
    );
    expect(screen.getByTestId("remove-progress").textContent).toContain("60");
    toastWarn.mockClear();
    act(() => listeners.get("ai-fill-notice")!({ payload: { taskId: "other", reason: "download" } }));
    expect(toastWarn).not.toHaveBeenCalled();
    act(() => listeners.get("ai-fill-notice")!({ payload: { taskId, reason: "download" } }));
    expect(toastWarn).toHaveBeenCalledWith(expect.stringContaining("MI-GAN"), expect.anything());
    // Moebius too slow on this GPU: said once for the session, whatever the task.
    toastInfo.mockClear();
    act(() => listeners.get("ai-fill-notice")!({ payload: { taskId: null, reason: "tooSlow" } }));
    expect(toastInfo).toHaveBeenCalledTimes(1);
  });

  it("cancels while preparing the GPU without adding a patch", async () => {
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
    act(() => listeners.get("ai-fill-progress")!({ payload: { taskId, phase: "preparing", done: 0, total: 0 } }));
    expect(screen.getByTestId("remove-progress").textContent).toContain("Preparing large-area removal");
    fireEvent.click(screen.getByRole("button"));
    expect(invoke).toHaveBeenCalledWith("cancel_ai_task", { taskId });
    await act(async () => failFill("Task cancelled"));
    expect(screen.queryByTestId("remove-progress")).toBeNull();
    expect(useEditorStore.getState().adjustments.aiPatches ?? []).toHaveLength(0);
  });

  it("keeps a slow removal's progress and Cancel when a quicker one finishes meanwhile", async () => {
    const patchData = { color: "c", mask: "m", offsetX: 0, offsetY: 0, width: 4, height: 4, isSrgbEncoded: true };
    let fills = 0;
    invoke.mockImplementation((command: string) => {
      if (command !== "invoke_generative_replace_with_mask_def") return Promise.resolve();
      fills += 1;
      // The first fill is slow (pending), the second finishes at once.
      return fills === 1 ? new Promise(() => {}) : Promise.resolve(JSON.stringify(patchData));
    });
    useEditorStore.setState({ spotSettings: { mode: "remove", size: 20, feather: 0, opacity: 100 } });
    const el = overlay();
    await act(async () => {
      fireEvent.pointerDown(el, { button: 0, clientX: 50, clientY: 50, pointerId: 1 });
      fireEvent.pointerUp(el, { clientX: 50, clientY: 50, pointerId: 1 });
    });
    const slow = (invoke.mock.calls.find(([name]) => name === "invoke_generative_replace_with_mask_def")![1] as {
      taskId: string;
    }).taskId;
    act(() => listeners.get("ai-fill-progress")!({ payload: { taskId: slow, done: 2, total: 9 } }));
    await act(async () => {
      fireEvent.pointerDown(el, { button: 0, clientX: 150, clientY: 100, pointerId: 2 });
      fireEvent.pointerUp(el, { clientX: 150, clientY: 100, pointerId: 2 });
    });
    expect(useEditorStore.getState().adjustments.aiPatches).toHaveLength(1);
    expect(screen.getByTestId("remove-progress").textContent).toMatch(/2\D+9/);
    act(() => listeners.get("ai-fill-progress")!({ payload: { taskId: slow, done: 3, total: 9 } }));
    expect(screen.getByTestId("remove-progress").textContent).toMatch(/3\D+9/);
    fireEvent.keyDown(window, { key: "Escape" });
    expect(invoke).toHaveBeenCalledWith("cancel_ai_task", { taskId: slow });
  });

  it("shows the newest of two running removals and keeps it when the older one finishes", async () => {
    const patchData = { color: "c", mask: "m", offsetX: 0, offsetY: 0, width: 4, height: 4, isSrgbEncoded: true };
    const finish: Array<(value: unknown) => void> = [];
    invoke.mockImplementation((command: string) =>
      command === "invoke_generative_replace_with_mask_def"
        ? new Promise((resolve) => finish.push(resolve))
        : Promise.resolve(),
    );
    useEditorStore.setState({ spotSettings: { mode: "remove", size: 20, feather: 0, opacity: 100 } });
    const el = overlay();
    for (const [x, id] of [[50, 1], [150, 2]]) {
      await act(async () => {
        fireEvent.pointerDown(el, { button: 0, clientX: x, clientY: 50, pointerId: id });
        fireEvent.pointerUp(el, { clientX: x, clientY: 50, pointerId: id });
      });
    }
    const [a, b] = invoke.mock.calls
      .filter(([name]) => name === "invoke_generative_replace_with_mask_def")
      .map(([, args]) => (args as { taskId: string }).taskId);
    act(() => listeners.get("ai-fill-progress")!({ payload: { taskId: a, done: 2, total: 9 } }));
    act(() => listeners.get("ai-fill-progress")!({ payload: { taskId: b, done: 4, total: 9 } }));
    expect(screen.getByTestId("remove-progress").textContent).toMatch(/4\D+9/);
    // The older one reports its last step and finishes: the newer one stays shown.
    act(() => listeners.get("ai-fill-progress")!({ payload: { taskId: a, done: 9, total: 9 } }));
    expect(screen.getByTestId("remove-progress").textContent).toMatch(/4\D+9/);
    await act(async () => finish[0](JSON.stringify(patchData)));
    expect(screen.getByTestId("remove-progress").textContent).toMatch(/4\D+9/);
    fireEvent.click(screen.getByRole("button"));
    expect(invoke).toHaveBeenCalledWith("cancel_ai_task", { taskId: b });
    await act(async () => finish[1](JSON.stringify(patchData)));
    expect(screen.queryByTestId("remove-progress")).toBeNull();
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
