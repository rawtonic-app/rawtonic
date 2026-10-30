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

const { invoke, info } = vi.hoisted(() => ({ invoke: vi.fn(), info: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
vi.mock("../../../activity/notify", () => ({ notify: { info } }));

import RedEyeOverlay from "./RedEyeOverlay";
import { useEditorStore } from "../../../store/useEditorStore";
import {
  INITIAL_ADJUSTMENTS,
  NON_COPYABLE_ADJUSTMENT_KEYS,
} from "../../../utils/adjustments";

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
    spotSettings: { mode: "redEye", size: 20, feather: 0, opacity: 100 },
    redEyeSettings: {
      kind: "red",
      size: 30,
      pupilSize: 60,
      darken: 40,
      catchlight: true,
    },
  });
  invoke.mockReset();
  info.mockReset();
});
afterEach(cleanup);

// The box shows mask pixels 100…500 × 50…350 at half size (crop at 100, 50).
const overlay = () => {
  render(
    <RedEyeOverlay
      rect={{ left: 0, top: 0, width: 200, height: 150 }}
      cropX={100}
      cropY={50}
      scale={0.5}
      zoom={1}
    />,
  );
  const el = screen.getByTestId("red-eye-overlay");
  el.getBoundingClientRect = () =>
    ({ left: 0, top: 0, width: 200, height: 150 }) as DOMRect;
  return el;
};

const pupil = { x: 201, y: 149, radiusX: 6, radiusY: 5, angle: 10 };

describe("red eye overlay", () => {
  it("adds the pupil a click finds with the settings new corrections get", async () => {
    invoke.mockResolvedValue(pupil);
    const el = overlay();
    await act(async () => {
      fireEvent.pointerDown(el, {
        button: 0,
        clientX: 50,
        clientY: 50,
        pointerId: 1,
      });
      fireEvent.pointerUp(el, { clientX: 50, clientY: 50, pointerId: 1 });
    });
    expect(invoke).toHaveBeenCalledWith(
      "detect_red_eye",
      expect.objectContaining({ x: 200, y: 150, radius: 30, kind: "red" }),
    );
    const { adjustments, activeSpotId } = useEditorStore.getState();
    expect(adjustments.redEyes).toHaveLength(1);
    expect(adjustments.redEyes?.[0]).toMatchObject({
      version: 1,
      kind: "red",
      ...pupil,
      pupilSize: 60,
      darken: 40,
      // Only pet eyes get a catchlight.
      catchlight: false,
      visible: true,
    });
    expect(activeSpotId).toBe(adjustments.redEyes?.[0].id);
  });

  it("sizes the searched circle by a drag and says when nothing is found", async () => {
    invoke.mockResolvedValue(null);
    const el = overlay();
    await act(async () => {
      fireEvent.pointerDown(el, {
        button: 0,
        clientX: 50,
        clientY: 50,
        pointerId: 1,
      });
      // 20 box pixels right: 40 mask pixels.
      fireEvent.pointerMove(el, { clientX: 70, clientY: 50, pointerId: 1 });
      fireEvent.pointerUp(el, { clientX: 70, clientY: 50, pointerId: 1 });
    });
    expect(invoke).toHaveBeenCalledWith(
      "detect_red_eye",
      expect.objectContaining({ radius: 40 }),
    );
    expect(useEditorStore.getState().adjustments.redEyes).toHaveLength(0);
    expect(useEditorStore.getState().redEyeSettings.size).toBe(40);
    expect(info).toHaveBeenCalledWith("spotRemoval.redEye.noRedEye");
  });

  it("shows corrections where the backend says, moves them there and deletes them with Delete", async () => {
    const stored = {
      id: "e1",
      version: 1,
      kind: "pet" as const,
      x: 0.3,
      y: 0.2,
      radiusX: 0.002,
      radiusY: 0.002,
      angle: 0,
      pupilSize: 50,
      darken: 50,
      catchlight: true,
      catchlightX: -0.3,
      catchlightY: -0.35,
      visible: true,
    };
    useEditorStore.setState({
      adjustments: {
        ...structuredClone(INITIAL_ADJUSTMENTS),
        redEyes: [stored],
      },
    });
    invoke.mockImplementation(async (command: string) => {
      // Shown at mask (200, 150) with an 8 px radius; a drag's end lands at
      // (0.31, 0.22) of the photo.
      if (command === "red_eyes_shown")
        return [{ x: 200, y: 150, radiusX: 8, radiusY: 8, angle: 0 }];
      if (command === "red_eye_point") return [0.31, 0.22];
      return null;
    });
    const el = overlay();
    await act(async () => {});
    expect(screen.getAllByTestId("red-eye")).toHaveLength(1);
    await act(async () => {
      fireEvent.pointerDown(el, {
        button: 0,
        clientX: 50,
        clientY: 50,
        pointerId: 1,
      });
      fireEvent.pointerMove(el, { clientX: 55, clientY: 52, pointerId: 1 });
      fireEvent.pointerUp(el, { clientX: 55, clientY: 52, pointerId: 1 });
    });
    expect(invoke).not.toHaveBeenCalledWith(
      "detect_red_eye",
      expect.anything(),
    );
    expect(invoke).toHaveBeenCalledWith(
      "red_eye_point",
      expect.objectContaining({ x: 210, y: 154 }),
    );
    expect(useEditorStore.getState().adjustments.redEyes?.[0]).toMatchObject({
      x: 0.31,
      y: 0.22,
      radiusX: 0.002,
    });
    expect(useEditorStore.getState().activeSpotId).toBe("e1");
    await act(async () => {
      fireEvent.keyDown(window, { key: "Delete" });
    });
    expect(useEditorStore.getState().adjustments.redEyes).toHaveLength(0);
  });

  it("selects a correction by a right-click and deletes one by an Alt-click, searching nothing", async () => {
    const stored = (id: string, visible: boolean) => ({
      id,
      version: 1,
      kind: "red" as const,
      x: 0.3,
      y: 0.2,
      radiusX: 0.002,
      radiusY: 0.002,
      angle: 0,
      pupilSize: 50,
      darken: 50,
      catchlight: false,
      catchlightX: 0,
      catchlightY: 0,
      visible,
    });
    useEditorStore.setState({
      adjustments: {
        ...structuredClone(INITIAL_ADJUSTMENTS),
        redEyes: [stored("e1", false), stored("e2", true)],
      },
    });
    // e1 at mask (200, 150), e2 at (400, 250), 8 px radius.
    invoke.mockImplementation(async (command: string) =>
      command === "red_eyes_shown"
        ? [
            { x: 200, y: 150, radiusX: 8, radiusY: 8, angle: 0 },
            { x: 400, y: 250, radiusX: 8, radiusY: 8, angle: 0 },
          ]
        : null,
    );
    const el = overlay();
    await act(async () => {});
    // The hidden correction is drawn faintly.
    expect(screen.getAllByTestId("red-eye").map((g) => g.getAttribute("opacity"))).toEqual(["0.4", null]);
    invoke.mockClear();
    fireEvent.pointerDown(el, { button: 2, clientX: 150, clientY: 100, pointerId: 1 });
    expect(useEditorStore.getState().activeSpotId).toBe("e2");
    // A cancelled Alt press deletes nothing; a released one deletes.
    fireEvent.pointerDown(el, { button: 0, altKey: true, clientX: 150, clientY: 100, pointerId: 1 });
    fireEvent.pointerCancel(el, { clientX: 150, clientY: 100, pointerId: 1 });
    expect(useEditorStore.getState().adjustments.redEyes).toHaveLength(2);
    fireEvent.pointerDown(el, { button: 0, altKey: true, clientX: 150, clientY: 100, pointerId: 1 });
    fireEvent.pointerUp(el, { clientX: 150, clientY: 100, pointerId: 1 });
    expect(useEditorStore.getState().adjustments.redEyes!.map((e) => e.id)).toEqual(["e1"]);
    expect(useEditorStore.getState().activeSpotId).toBeNull();
    // An Alt-click beside every correction does nothing.
    await act(async () => {
      fireEvent.pointerDown(el, { button: 0, altKey: true, clientX: 10, clientY: 140, pointerId: 1 });
      fireEvent.pointerUp(el, { clientX: 10, clientY: 140, pointerId: 1 });
    });
    expect(invoke).not.toHaveBeenCalledWith("detect_red_eye", expect.anything());
    expect(useEditorStore.getState().adjustments.redEyes).toHaveLength(1);
  });

  it("selects a correction anywhere inside its outline at its Pupil Size", async () => {
    const stored = {
      id: "e1",
      version: 1,
      kind: "red" as const,
      x: 0.3,
      y: 0.2,
      radiusX: 0.004,
      radiusY: 0.004,
      angle: 0,
      pupilSize: 100,
      darken: 50,
      catchlight: false,
      catchlightX: -0.3,
      catchlightY: -0.35,
      visible: true,
    };
    useEditorStore.setState({
      adjustments: {
        ...structuredClone(INITIAL_ADJUSTMENTS),
        redEyes: [stored],
      },
    });
    invoke.mockImplementation(async (command: string) =>
      // Shown at mask (200, 150) with a 20 px radius: at Pupil Size 100 its
      // outline is 28 px out.
      command === "red_eyes_shown"
        ? [{ x: 200, y: 150, radiusX: 20, radiusY: 20, angle: 0 }]
        : null,
    );
    const el = overlay();
    await act(async () => {});
    // 27 mask px right of the centre (13.5 box px): inside the outline.
    await act(async () => {
      fireEvent.pointerDown(el, {
        button: 0,
        clientX: 63.5,
        clientY: 50,
        pointerId: 1,
      });
      fireEvent.pointerUp(el, { clientX: 63.5, clientY: 50, pointerId: 1 });
    });
    expect(invoke).not.toHaveBeenCalledWith(
      "detect_red_eye",
      expect.anything(),
    );
    expect(useEditorStore.getState().activeSpotId).toBe("e1");
    // At Pupil Size 0 (outline 12 px out) the same press searches anew.
    useEditorStore.setState({ activeSpotId: null });
    useEditorStore.setState({
      adjustments: {
        ...structuredClone(INITIAL_ADJUSTMENTS),
        redEyes: [{ ...stored, pupilSize: 0 }],
      },
    });
    await act(async () => {});
    await act(async () => {
      fireEvent.pointerDown(el, {
        button: 0,
        clientX: 63.5,
        clientY: 50,
        pointerId: 2,
      });
      fireEvent.pointerUp(el, { clientX: 63.5, clientY: 50, pointerId: 2 });
    });
    expect(invoke).toHaveBeenCalledWith("detect_red_eye", expect.anything());
  });

  it("changes the search circle with [ and ] by at least a pixel", async () => {
    useEditorStore.setState({
      redEyeSettings: {
        kind: "red",
        size: 2,
        pupilSize: 50,
        darken: 50,
        catchlight: true,
      },
    });
    overlay();
    const size = () => useEditorStore.getState().redEyeSettings.size;
    await act(async () => {
      fireEvent.keyDown(window, { key: "]" });
    });
    expect(size()).toBe(3);
    await act(async () => {
      fireEvent.keyDown(window, { key: "]" });
    });
    expect(size()).toBe(4);
    await act(async () => {
      fireEvent.keyDown(window, { key: "[" });
      fireEvent.keyDown(window, { key: "[" });
      fireEvent.keyDown(window, { key: "[" });
    });
    expect(size()).toBe(2);
  });

  it("keeps red eye corrections with their photo when settings are copied", () => {
    expect(NON_COPYABLE_ADJUSTMENT_KEYS).toContain("redEyes");
  });
});
