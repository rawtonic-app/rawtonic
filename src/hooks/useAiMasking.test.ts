import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const { invoke, listen, listeners } = vi.hoisted(() => {
  const listeners = new Map<string, (event: { payload: unknown }) => void>();
  return {
    invoke: vi.fn(),
    listeners,
    listen: vi.fn(async (name: string, handler: (event: { payload: unknown }) => void) => {
      listeners.set(name, handler);
      return () => listeners.delete(name);
    }),
  };
});
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen }));
const { toast } = vi.hoisted(() => ({ toast: { error: vi.fn() } }));
vi.mock("react-toastify", () => ({ toast }));

import "../i18n";
import {
  AI_MASKING_RECENT_MS,
  AI_MASKS_AHEAD_DELAY_MS,
  AI_MODELS_UNLOADED_EVENT,
  openMaskingTools,
  resetAiMaskSession,
  useAiMasking,
  useAiModelLifecycle,
} from "./useAiMasking";
import { useEditorStore } from "../store/useEditorStore";
import { useProcessStore } from "../store/useProcessStore";
import { useUIStore } from "../store/useUIStore";
import { Panel } from "../components/ui/AppProperties";
import { INITIAL_ADJUSTMENTS } from "../utils/adjustments";
import { updateAdjustments } from "./useEditorActions";
import { registerSliderDraftRenderer } from "../utils/sliderDraft";

const layer = (type: string, parameters: Record<string, unknown>) => ({
  id: `layer-${type}`,
  name: "Imported",
  visible: true,
  invert: false,
  opacity: 100,
  adjustments: {},
  subMasks: [
    {
      id: `sub-${type}`,
      type,
      visible: true,
      invert: false,
      opacity: 100,
      mode: "additive",
      parameters,
    },
  ],
});

beforeEach(() => {
  resetAiMaskSession();
  localStorage.clear();
  invoke.mockReset();
  invoke.mockResolvedValue({ maskDataBase64: "data:image/png;base64,AAAA" });
});
afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

describe("automatic AI masks from imported presets", () => {
  it("computes sky, subject and depth masks that have no data yet, once each", async () => {
    useEditorStore.setState({
      selectedImage: { path: "/s/a.NEF", isReady: true, isRaw: true } as never,
      adjustments: {
        ...structuredClone(INITIAL_ADJUSTMENTS),
        masks: [
          layer("ai-sky", { maskDataBase64: null, grow: 0, feather: 0 }),
          layer("ai-foreground", { maskDataBase64: null, grow: 0, feather: 0 }),
          layer("ai-depth", { maskDataBase64: null }),
        ] as never,
      },
    });
    const generated = () => invoke.mock.calls.map((c) => c[0]).filter((c) => String(c).startsWith("generate_"));
    const { rerender } = renderHook(() => useAiMasking());
    await waitFor(() => expect(generated()).toHaveLength(3));
    expect(generated()).toContain("generate_ai_sky_mask");
    expect(generated()).toContain("generate_ai_foreground_mask");
    expect(generated()).toContain("generate_ai_depth_mask");
    await waitFor(() => {
      const subs = useEditorStore
        .getState()
        .adjustments.masks.flatMap((m) => m.subMasks);
      expect(subs.find((s) => s.id === "sub-ai-sky")?.parameters).toMatchObject(
        {
          maskDataBase64: "data:image/png;base64,AAAA",
        },
      );
    });
    rerender();
    expect(generated()).toHaveLength(3);
  });

  /** Answers `generate_*` calls only when the test resolves them. */
  function deferredMasks() {
    const pending: ((value: unknown) => void)[] = [];
    invoke.mockImplementation((command: string) =>
      String(command).startsWith("generate_")
        ? new Promise((resolve) => pending.push(resolve))
        : Promise.resolve(null),
    );
    return pending;
  }
  const subOf = (id: string) =>
    useEditorStore
      .getState()
      .adjustments.masks.flatMap((m) => m.subMasks)
      .find((s) => s.id === id);

  it("keeps settings changed while a mask computes", async () => {
    const pending = deferredMasks();
    useEditorStore.setState({
      selectedImage: { path: "/s/a.NEF", isReady: true, isRaw: true } as never,
      adjustments: {
        ...structuredClone(INITIAL_ADJUSTMENTS),
        masks: [layer("ai-sky", { maskDataBase64: null, grow: 0, feather: 0 })] as never,
      },
    });
    renderHook(() => useAiMasking());
    await waitFor(() => expect(pending).toHaveLength(1));
    act(() =>
      useEditorStore.setState((state) => ({
        adjustments: {
          ...state.adjustments,
          masks: state.adjustments.masks.map((m) => ({
            ...m,
            subMasks: m.subMasks.map((s) => ({ ...s, parameters: { ...s.parameters, grow: 30, feather: 20 } })),
          })),
        },
      })),
    );
    await act(async () => pending[0]({ maskDataBase64: "data:image/png;base64,AAAA", rotation: 0 }));
    expect(subOf("sub-ai-sky")?.parameters).toMatchObject({
      maskDataBase64: "data:image/png;base64,AAAA",
      grow: 30,
      feather: 20,
    });
  });

  it("keeps a depth range's limits changed while its depth map computes", async () => {
    const pending = deferredMasks();
    useEditorStore.setState({
      selectedImage: { path: "/s/a.NEF", isReady: true, isRaw: true } as never,
      adjustments: {
        ...structuredClone(INITIAL_ADJUSTMENTS),
        masks: [layer("ai-depth", { maskDataBase64: null, minDepth: 20, maxDepth: 100 })] as never,
      },
    });
    renderHook(() => useAiMasking());
    await waitFor(() => expect(pending).toHaveLength(1));
    act(() =>
      useEditorStore.setState((state) => ({
        adjustments: {
          ...state.adjustments,
          masks: state.adjustments.masks.map((m) => ({
            ...m,
            subMasks: m.subMasks.map((s) => ({ ...s, parameters: { ...s.parameters, minDepth: 45 } })),
          })),
        },
      })),
    );
    await act(async () =>
      pending[0]({ maskDataBase64: "data:image/png;base64,AAAA", minDepth: 20, maxDepth: 100, feather: 10 }),
    );
    expect(subOf("sub-ai-depth")?.parameters).toMatchObject({
      maskDataBase64: "data:image/png;base64,AAAA",
      minDepth: 45,
      maxDepth: 100,
      feather: 10,
    });
  });

  it("keeps a depth limit dragged on a slider while the depth map computes", async () => {
    const pending = deferredMasks();
    useEditorStore.setState({
      selectedImage: { path: "/s/a.NEF", isReady: true, isRaw: true, width: 4000, height: 3000 } as never,
      adjustments: {
        ...structuredClone(INITIAL_ADJUSTMENTS),
        masks: [layer("ai-depth", { maskDataBase64: null, minDepth: 20, maxDepth: 100 })] as never,
      },
    });
    renderHook(() => useAiMasking());
    await waitFor(() => expect(pending).toHaveLength(1));
    // The drag's moves stay a draft until it ends (sliderDraft.ts).
    const unregister = registerSliderDraftRenderer(() => true);
    act(() => {
      useEditorStore.setState({ isSliderDragging: true });
      updateAdjustments((prev) => ({
        ...prev,
        masks: prev.masks.map((m) => ({
          ...m,
          subMasks: m.subMasks.map((s) => ({ ...s, parameters: { ...s.parameters, minDepth: 45 } })),
        })),
      }));
    });
    expect(subOf("sub-ai-depth")?.parameters).toMatchObject({ minDepth: 20 });
    await act(async () =>
      pending[0]({ maskDataBase64: "data:image/png;base64,AAAA", minDepth: 20, maxDepth: 100 }),
    );
    act(() => useEditorStore.setState({ isSliderDragging: false }));
    unregister();
    expect(subOf("sub-ai-depth")?.parameters).toMatchObject({
      maskDataBase64: "data:image/png;base64,AAAA",
      minDepth: 45,
    });
  });

  it("leaves another photo's component alone when a mask finishes after the photo changed", async () => {
    const pending = deferredMasks();
    useEditorStore.setState({
      selectedImage: { path: "/s/a.NEF", isReady: true, isRaw: true } as never,
      adjustments: {
        ...structuredClone(INITIAL_ADJUSTMENTS),
        masks: [layer("ai-sky", { maskDataBase64: null, grow: 0, feather: 0 })] as never,
      },
    });
    renderHook(() => useAiMasking());
    await waitFor(() => expect(pending).toHaveLength(1));
    // Settings pasted from the first photo keep its component ids.
    act(() =>
      useEditorStore.setState({
        selectedImage: { path: "/s/b.NEF", isReady: true, isRaw: true } as never,
        adjustments: {
          ...structuredClone(INITIAL_ADJUSTMENTS),
          masks: [layer("ai-sky", { maskDataBase64: "data:image/png;base64,BBBB", grow: 0, feather: 0 })] as never,
        },
      }),
    );
    await act(async () => pending[0]({ maskDataBase64: "data:image/png;base64,AAAA" }));
    expect(subOf("sub-ai-sky")?.parameters).toMatchObject({ maskDataBase64: "data:image/png;base64,BBBB" });
  });

  it("computes a mask again on returning to its photo when the first result came after leaving it", async () => {
    const pending = deferredMasks();
    const photoA = {
      selectedImage: { path: "/s/a.NEF", isReady: true, isRaw: true } as never,
      adjustments: {
        ...structuredClone(INITIAL_ADJUSTMENTS),
        masks: [layer("ai-sky", { maskDataBase64: null, grow: 0, feather: 0 })] as never,
      },
    };
    useEditorStore.setState(photoA);
    renderHook(() => useAiMasking());
    await waitFor(() => expect(pending).toHaveLength(1));
    act(() =>
      useEditorStore.setState({
        selectedImage: { path: "/s/b.NEF", isReady: true, isRaw: true } as never,
        adjustments: structuredClone(INITIAL_ADJUSTMENTS),
      }),
    );
    await act(async () => pending[0]({ maskDataBase64: "data:image/png;base64,AAAA" }));
    act(() => useEditorStore.setState(structuredClone(photoA)));
    await waitFor(() => expect(pending).toHaveLength(2));
    await act(async () => pending[1]({ maskDataBase64: "data:image/png;base64,CCCC" }));
    expect(subOf("sub-ai-sky")?.parameters).toMatchObject({ maskDataBase64: "data:image/png;base64,CCCC" });
  });

  it("computes a mask again when its result came while its photo was opening again", async () => {
    const pending = deferredMasks();
    const photoA = (isReady: boolean) => ({
      selectedImage: { path: "/s/a.NEF", isReady, isRaw: true } as never,
      adjustments: {
        ...structuredClone(INITIAL_ADJUSTMENTS),
        masks: [layer("ai-sky", { maskDataBase64: null, grow: 0, feather: 0 })] as never,
      },
    });
    useEditorStore.setState(photoA(true));
    renderHook(() => useAiMasking());
    await waitFor(() => expect(pending).toHaveLength(1));
    act(() => useEditorStore.setState(photoA(false)));
    await act(async () => pending[0]({ maskDataBase64: "data:image/png;base64,AAAA" }));
    expect(subOf("sub-ai-sky")?.parameters).toMatchObject({ maskDataBase64: null });
    act(() => useEditorStore.setState({ selectedImage: { path: "/s/a.NEF", isReady: true, isRaw: true } as never }));
    await waitFor(() => expect(pending).toHaveLength(2));
  });

  it("asks again, without a toast, when the backend had another photo loaded (A, B, back to A)", async () => {
    const asked: { path?: string; reject: (error: unknown) => void; resolve: (value: unknown) => void }[] = [];
    invoke.mockImplementation((command: string, args?: { path?: string }) =>
      String(command).startsWith("generate_")
        ? new Promise((resolve, reject) => asked.push({ path: args?.path, resolve, reject }))
        : Promise.resolve(null),
    );
    useEditorStore.setState({
      selectedImage: { path: "/s/a.NEF", isReady: true, isRaw: true } as never,
      adjustments: {
        ...structuredClone(INITIAL_ADJUSTMENTS),
        masks: [layer("ai-sky", { maskDataBase64: null, grow: 0, feather: 0 })] as never,
      },
    });
    renderHook(() => useAiMasking());
    await waitFor(() => expect(asked).toHaveLength(1));
    expect(asked[0].path).toBe("/s/a.NEF");
    // Meanwhile B was opened and A again; the backend computed on B.
    await act(async () => asked[0].reject("The photo changed while its mask was asked for"));
    await waitFor(() => expect(asked).toHaveLength(2));
    expect(toast.error).not.toHaveBeenCalled();
    await act(async () => asked[1].resolve({ maskDataBase64: "data:image/png;base64,DDDD" }));
    expect(subOf("sub-ai-sky")?.parameters).toMatchObject({ maskDataBase64: "data:image/png;base64,DDDD" });
  });

  it("stays busy until the last of two requests under one component id ends", () => {
    const { registerAiTask, unregisterAiTask, isAiTaskRunning } = useProcessStore.getState();
    useProcessStore.setState({ activeAiTasks: {} });
    registerAiTask("sub-ai-sky", "sky");
    registerAiTask("sub-ai-sky", "sky");
    unregisterAiTask("sub-ai-sky");
    expect(isAiTaskRunning("sub-ai-sky")).toBe(true);
    unregisterAiTask("sub-ai-sky");
    expect(isAiTaskRunning("sub-ai-sky")).toBe(false);
  });

  it("computes a copied component's mask on another photo too", async () => {
    const pending = deferredMasks();
    const on = (path: string) => ({
      selectedImage: { path, isReady: true, isRaw: true } as never,
      adjustments: {
        ...structuredClone(INITIAL_ADJUSTMENTS),
        masks: [layer("ai-sky", { maskDataBase64: null, grow: 0, feather: 0 })] as never,
      },
    });
    useEditorStore.setState(on("/s/a.NEF"));
    renderHook(() => useAiMasking());
    await waitFor(() => expect(pending).toHaveLength(1));
    await act(async () => pending[0]({ maskDataBase64: "data:image/png;base64,AAAA" }));
    act(() => useEditorStore.setState(on("/s/b.NEF")));
    await waitFor(() => expect(pending).toHaveLength(2));
  });

  it("computes a pasted component again when a later paste clears its data", async () => {
    useEditorStore.setState({
      selectedImage: { path: "/s/a.NEF", isReady: true, isRaw: true } as never,
      adjustments: {
        ...structuredClone(INITIAL_ADJUSTMENTS),
        masks: [layer("ai-sky", { maskDataBase64: null, detectPending: true })] as never,
      },
    });
    const generated = () => invoke.mock.calls.filter((c) => c[0] === "generate_ai_sky_mask");
    renderHook(() => useAiMasking());
    await waitFor(() => expect(subOf("sub-ai-sky")?.parameters).toMatchObject({ maskDataBase64: "data:image/png;base64,AAAA" }));
    // Detected here: no longer waiting.
    expect(subOf("sub-ai-sky")?.parameters).not.toHaveProperty("detectPending");
    // The same layer pasted again: the data is cleared, it is detected again.
    act(() =>
      useEditorStore.setState({
        adjustments: {
          ...structuredClone(INITIAL_ADJUSTMENTS),
          masks: [layer("ai-sky", { maskDataBase64: null, detectPending: true })] as never,
        },
      }),
    );
    await waitFor(() => expect(generated()).toHaveLength(2));
  });

  it("detects a pasted Object selection by its box, scaled from the photo it was pointed on", async () => {
    const pending = deferredMasks();
    const object = () =>
      layer("ai-subject", {
        startX: 600, startY: 400, endX: 1200, endY: 800,
        maskDataBase64: null, detectPending: true, sourceSize: { width: 6000, height: 4000 },
      });
    const on = (path: string) => ({
      selectedImage: { path, isReady: true, isRaw: true, width: 3000, height: 2000 } as never,
      adjustments: { ...structuredClone(INITIAL_ADJUSTMENTS), masks: [object()] as never },
    });
    useEditorStore.setState(on("/s/a.NEF"));
    renderHook(() => useAiMasking());
    const subjectCalls = () => invoke.mock.calls.filter((c) => c[0] === "generate_ai_subject_mask");
    await waitFor(() => expect(subjectCalls()).toHaveLength(1));
    expect(subjectCalls()[0][1]).toMatchObject({ startPoint: [300, 200], endPoint: [600, 400] });
    // Left before it answered: asked again on returning to the photo.
    act(() => useEditorStore.setState(on("/s/b.NEF")));
    await act(async () => pending[0]({ maskDataBase64: "data:image/png;base64,AAAA", startX: 300, startY: 200, endX: 600, endY: 400 }));
    act(() => useEditorStore.setState(on("/s/a.NEF")));
    await waitFor(() => expect(subjectCalls().filter((c) => (c[1] as { path: string }).path === "/s/a.NEF")).toHaveLength(2));
    const asked = pending.length;
    await act(async () => pending[asked - 1]({ maskDataBase64: "data:image/png;base64,BBBB", startX: 300, startY: 200, endX: 600, endY: 400 }));
    // Detected: its box is this photo's now, without the source's size.
    await waitFor(() => expect(subOf("sub-ai-subject")?.parameters).toMatchObject({ maskDataBase64: "data:image/png;base64,BBBB" }));
    expect(subOf("sub-ai-subject")?.parameters).not.toHaveProperty("sourceSize");
    expect(subOf("sub-ai-subject")?.parameters).not.toHaveProperty("detectPending");
  });

  it("clears the busy state and says so in a toast when the AI runtime cannot be loaded", async () => {
    toast.error.mockReset();
    useProcessStore.setState({ activeAiTasks: {} });
    useEditorStore.setState({
      selectedImage: { path: "/s/a.NEF", isReady: true, isRaw: true } as never,
      adjustments: {
        ...structuredClone(INITIAL_ADJUSTMENTS),
        masks: [
          layer("ai-foreground", { maskDataBase64: null, grow: 0, feather: 0 }),
          layer("ai-sky", { maskDataBase64: null, grow: 0, feather: 0 }),
        ] as never,
      },
    });
    const failures: ((error: string) => void)[] = [];
    invoke.mockImplementation(
      (command: string) =>
        new Promise((_, reject) => {
          if (command.startsWith("generate_")) failures.push(reject);
        }),
    );
    renderHook(() => useAiMasking());
    // Both components are computing ("computing…" on their rows).
    await waitFor(() => expect(failures).toHaveLength(2));
    expect(Object.keys(useProcessStore.getState().activeAiTasks).sort()).toEqual([
      "sub-ai-foreground",
      "sub-ai-sky",
    ]);
    // The backend's error for a missing or broken ONNX Runtime.
    const error =
      "AI runtime unavailable: /app/resources/libonnxruntime.dylib could not be loaded: not a library";
    act(() => failures.forEach((reject) => reject(error)));
    await waitFor(() => expect(useProcessStore.getState().activeAiTasks).toEqual({}));
    expect(toast.error).toHaveBeenCalledTimes(2);
    for (const [text, options] of toast.error.mock.calls) {
      expect(text).toBe(
        "The AI runtime could not be loaded, so AI tools are unavailable. Everything else works as usual.",
      );
      // One toast on screen for both.
      expect(options).toEqual({ toastId: "ai-runtime-unavailable" });
    }
  });

  it("computes a People mask again when its parts or person change", async () => {
    useEditorStore.setState({
      selectedImage: { path: "/s/a.NEF", isReady: true, isRaw: true } as never,
      adjustments: {
        ...structuredClone(INITIAL_ADJUSTMENTS),
        masks: [layer("ai-people", { maskDataBase64: null, parts: ["faceSkin"], person: 1 })] as never,
      },
    });
    const { rerender } = renderHook(() => useAiMasking());
    await waitFor(() => expect(invoke).toHaveBeenCalledTimes(1));
    expect(invoke.mock.calls[0][0]).toBe("generate_ai_people_mask");
    expect(invoke.mock.calls[0][1]).toMatchObject({ path: "/s/a.NEF", parts: ["faceSkin"], person: 1 });
    await waitFor(() =>
      expect(useEditorStore.getState().adjustments.masks[0].subMasks[0].parameters).toMatchObject({
        maskDataBase64: "data:image/png;base64,AAAA",
        parts: ["faceSkin"],
      }),
    );
    rerender();
    expect(invoke).toHaveBeenCalledTimes(1);
    // The panel clears the data with a new choice.
    useEditorStore.setState((state) => ({
      adjustments: {
        ...state.adjustments,
        masks: [layer("ai-people", { maskDataBase64: null, parts: ["faceSkin", "lips"], person: 1 })] as never,
      },
    }));
    await waitFor(() => expect(invoke).toHaveBeenCalledTimes(2));
    expect(invoke.mock.calls[1][1]).toMatchObject({ parts: ["faceSkin", "lips"], person: 1 });
  });

  it("has the backend place imported Lightroom geometry once, in the open photo", async () => {
    const placed = {
      centerX: 3000,
      centerY: 1600,
      radiusX: 1200,
      radiusY: 800,
      rotation: 0,
      feather: 0.5,
    };
    invoke.mockImplementation(async (command: string, args: { adjustments: { masks: { subMasks: { id: string }[] }[] } }) =>
      command === "place_lightroom_masks"
        ? {
            masks: [
              {
                subMasks: args.adjustments.masks[0].subMasks.map((sub) => ({
                  ...sub,
                  parameters: placed,
                })),
              },
            ],
          }
        : null,
    );
    useEditorStore.setState({
      selectedImage: {
        path: "/s/a.NEF",
        isReady: true,
        isRaw: true,
        width: 6000,
        height: 4000,
      } as never,
      adjustments: {
        ...structuredClone(INITIAL_ADJUSTMENTS),
        masks: [
          layer("radial", {
            lrNormalized: {
              top: 0.2,
              left: 0.3,
              bottom: 0.6,
              right: 0.7,
              angle: 0,
              feather: 50,
            },
          }),
        ] as never,
      },
    });
    const { rerender } = renderHook(() => useAiMasking());
    await waitFor(() => {
      const sub = useEditorStore.getState().adjustments.masks[0].subMasks[0];
      expect(sub.parameters).toEqual(placed);
    });
    expect(invoke).toHaveBeenCalledTimes(1);
    expect(invoke.mock.calls[0][0]).toBe("place_lightroom_masks");
    expect(invoke.mock.calls[0][1]).toMatchObject({
      path: "/s/a.NEF",
      width: 6000,
      height: 4000,
    });
    rerender();
    expect(invoke).toHaveBeenCalledTimes(1);
  });

  it("leaves masks that already carry data alone", async () => {
    useEditorStore.setState({
      selectedImage: { path: "/s/a.NEF", isReady: true, isRaw: true } as never,
      adjustments: {
        ...structuredClone(INITIAL_ADJUSTMENTS),
        masks: [layer("ai-sky", { maskDataBase64: "data:x" })] as never,
      },
    });
    renderHook(() => useAiMasking());
    await new Promise((r) => setTimeout(r, 20));
    expect(invoke).not.toHaveBeenCalled();
  });
});

const photo = (path: string) =>
  useEditorStore.setState({
    selectedImage: { path, isReady: true, isRaw: true } as never,
    adjustments: { ...structuredClone(INITIAL_ADJUSTMENTS), masks: [] },
  });
const calls = (command: string) => invoke.mock.calls.filter((c) => c[0] === command);
const pathsOf = (command: string) => calls(command).map((c) => (c[1] as { path: string }).path);
const wait = (ms: number) => new Promise((r) => setTimeout(r, ms));
/** Past the delay after which an open photo gets its masks ahead. */
const AFTER_DELAY = AI_MASKS_AHEAD_DELAY_MS + 200;

describe("Subject computed ahead when the masking tools open", () => {
  afterEach(() => {
    resetAiMaskSession();
    localStorage.clear();
    act(() => useUIStore.setState({ activePanel: Panel.Adjustments }));
  });

  it("starts once the masking tools open, not on opening a photo", async () => {
    // The background run stays open until the photo changes.
    invoke.mockImplementation((command: string) =>
      command === "precompute_ai_foreground_mask" ? new Promise(() => {}) : Promise.resolve(null),
    );
    useUIStore.setState({ activePanel: Panel.Adjustments });
    photo("/s/a.NEF");
    const { rerender } = renderHook(() => useAiMasking());
    await wait(20);
    expect(calls("precompute_ai_foreground_mask")).toHaveLength(0);

    act(() => useUIStore.setState({ activePanel: Panel.Masks }));
    await waitFor(() => expect(calls("precompute_ai_foreground_mask")).toHaveLength(1));
    const first = calls("precompute_ai_foreground_mask")[0][1] as { path: string; taskId: string };
    expect(first.path).toBe("/s/a.NEF");
    // The layer panel's buttons ask again: the same photo is not run twice.
    openMaskingTools();
    rerender();
    expect(calls("precompute_ai_foreground_mask")).toHaveLength(1);

    // Another photo: the running one is cancelled, the new one starts.
    act(() => photo("/s/b.NEF"));
    await waitFor(() => expect(calls("precompute_ai_foreground_mask")).toHaveLength(2));
    expect(calls("cancel_ai_task")).toEqual([["cancel_ai_task", { taskId: first.taskId }]]);
    expect((calls("precompute_ai_foreground_mask")[1][1] as { path: string }).path).toBe("/s/b.NEF");
  });

  it("cancels the object-mask encoding ahead when the photo changes", async () => {
    invoke.mockImplementation((command: string) =>
      command === "precompute_ai_subject_mask" ? new Promise(() => {}) : Promise.resolve(null),
    );
    photo("/s/a.NEF");
    renderHook(() => useAiMasking());
    act(() => useUIStore.setState({ activePanel: Panel.Masks }));
    await waitFor(() => expect(calls("precompute_ai_subject_mask")).toHaveLength(1));
    const encoding = calls("precompute_ai_subject_mask")[0][1] as { taskId: string; background: boolean };
    expect(encoding.background).toBe(true);
    expect(encoding.taskId).toMatch(/^sam-precompute-/);
    act(() => photo("/s/b.NEF"));
    await waitFor(() => expect(calls("cancel_ai_task")).toEqual([["cancel_ai_task", { taskId: encoding.taskId }]]));
  });

  it("computes Sky after Subject, for the same photo", async () => {
    invoke.mockResolvedValue(null);
    photo("/s/a.NEF");
    renderHook(() => useAiMasking());
    act(() => useUIStore.setState({ activePanel: Panel.Masks }));
    await waitFor(() => expect(calls("precompute_ai_sky_mask")).toHaveLength(1));
    const commands = invoke.mock.calls.map((c) => c[0]).filter((c) => String(c).startsWith("precompute_ai_"));
    expect(commands.indexOf("precompute_ai_foreground_mask")).toBeLessThan(commands.indexOf("precompute_ai_sky_mask"));
    expect((calls("precompute_ai_sky_mask")[0][1] as { path: string }).path).toBe("/s/a.NEF");
  });

  it("after the masking tools were used, computes ahead on opening a photo, with a short delay", async () => {
    invoke.mockResolvedValue(null);
    photo("/s/a.NEF");
    renderHook(() => useAiMasking());
    act(() => useUIStore.setState({ activePanel: Panel.Masks }));
    await waitFor(() => expect(calls("precompute_ai_sky_mask")).toHaveLength(1));
    act(() => useUIStore.setState({ activePanel: Panel.Adjustments }));

    act(() => photo("/s/b.NEF"));
    await wait(20);
    expect(calls("precompute_ai_foreground_mask")).toHaveLength(1);
    await waitFor(() => expect(calls("precompute_ai_foreground_mask")).toHaveLength(2), {
      timeout: AI_MASKS_AHEAD_DELAY_MS + 1000,
    });
    expect((calls("precompute_ai_foreground_mask")[1][1] as { path: string }).path).toBe("/s/b.NEF");
  });
});

describe("masking models loaded with Develop and dropped again", () => {
  afterEach(() => {
    resetAiMaskSession();
    localStorage.clear();
    act(() => useUIStore.setState({ activePanel: Panel.Adjustments }));
  });
  const usedBefore = () => localStorage.setItem("rawtonic.masks.used", "1");

  it("loads them with the first photo of a Develop visit when the masking tools were used before", async () => {
    usedBefore();
    invoke.mockResolvedValue(null);
    useUIStore.setState({ activePanel: Panel.Adjustments });
    photo("/s/a.NEF");
    renderHook(() => useAiMasking());
    // The photo's own masks are the warm-up: loading, then Subject and Sky.
    await waitFor(() => expect(pathsOf("precompute_ai_sky_mask")).toEqual(["/s/a.NEF"]), {
      timeout: AFTER_DELAY + 1000,
    });
    expect(pathsOf("precompute_ai_foreground_mask")).toEqual(["/s/a.NEF"]);
    // Opening a photo is not masking use.
    expect(calls("note_ai_mask_use")).toHaveLength(0);
    // Later photos without masking use are not computed ahead.
    act(() => photo("/s/b.NEF"));
    await wait(AFTER_DELAY);
    expect(pathsOf("precompute_ai_foreground_mask")).toEqual(["/s/a.NEF"]);
  });

  it("computes the visit's first photo at once; left before its run ends, the next photo that stays open gets it", async () => {
    usedBefore();
    // Subject runs until the photo changes.
    invoke.mockImplementation((command: string) =>
      command === "precompute_ai_foreground_mask" ? new Promise(() => {}) : Promise.resolve(null),
    );
    photo("/s/a.NEF");
    renderHook(() => useAiMasking());
    // At once, without the delay: that first run after loading is the slow one.
    await wait(20);
    expect(pathsOf("precompute_ai_foreground_mask")).toEqual(["/s/a.NEF"]);
    // Stepped on: the run is cancelled, photos stepped through quickly are skipped.
    act(() => photo("/s/b.NEF"));
    expect(calls("cancel_ai_task")).toHaveLength(1);
    await wait(AI_MASKS_AHEAD_DELAY_MS / 5);
    act(() => photo("/s/c.NEF"));
    await wait(AFTER_DELAY);
    expect(pathsOf("precompute_ai_foreground_mask")).toEqual(["/s/a.NEF", "/s/c.NEF"]);
  });
  it("loads nothing ahead when the masking tools were never used", async () => {
    invoke.mockResolvedValue(null);
    useUIStore.setState({ activePanel: Panel.Adjustments });
    photo("/s/a.NEF");
    renderHook(() => useAiMasking());
    await wait(1700);
    expect(calls("precompute_ai_foreground_mask")).toHaveLength(0);
    // Nor SAM's encoder for object masks.
    expect(calls("precompute_ai_subject_mask")).toHaveLength(0);
  });

  it("encodes for object masks in the background after Subject and Sky, only where those go ahead", async () => {
    usedBefore();
    invoke.mockResolvedValue(null);
    photo("/s/a.NEF");
    renderHook(() => useAiMasking());
    await waitFor(() => expect(calls("precompute_ai_subject_mask")).toHaveLength(1), { timeout: 2500 });
    expect(calls("precompute_ai_subject_mask")[0][1]).toMatchObject({ path: "/s/a.NEF", background: true });
    // Last, so the three models' working memory does not add up.
    const order = invoke.mock.calls.map((c) => c[0]).filter((c) => String(c).startsWith("precompute_ai_"));
    expect(order).toEqual(["precompute_ai_foreground_mask", "precompute_ai_sky_mask", "precompute_ai_subject_mask"]);
    act(() => photo("/s/b.NEF"));
    await wait(1700);
    expect(calls("precompute_ai_subject_mask")).toHaveLength(1);
  });

  it("stops computing ahead minutes after the last masking use", async () => {
    invoke.mockResolvedValue(null);
    const start = Date.now();
    const now = vi.spyOn(Date, "now").mockReturnValue(start);
    photo("/s/a.NEF");
    renderHook(() => useAiMasking());
    act(() => useUIStore.setState({ activePanel: Panel.Masks }));
    act(() => useUIStore.setState({ activePanel: Panel.Adjustments }));
    now.mockReturnValue(start + AI_MASKING_RECENT_MS - 1000);
    act(() => photo("/s/b.NEF"));
    await waitFor(() => expect(pathsOf("precompute_ai_foreground_mask")).toContain("/s/b.NEF"), {
      timeout: AFTER_DELAY + 1000,
    });
    now.mockReturnValue(start + AI_MASKING_RECENT_MS + 1000);
    act(() => photo("/s/c.NEF"));
    await wait(AFTER_DELAY);
    expect(pathsOf("precompute_ai_foreground_mask")).not.toContain("/s/c.NEF");
  });

  it("reports masking use to the backend at most every half minute", () => {
    invoke.mockResolvedValue(null);
    const start = Date.now();
    const now = vi.spyOn(Date, "now").mockReturnValue(start);
    openMaskingTools();
    openMaskingTools();
    expect(calls("note_ai_mask_use")).toHaveLength(1);
    now.mockReturnValue(start + 31_000);
    openMaskingTools();
    expect(calls("note_ai_mask_use")).toHaveLength(2);
  });

  it("showing the Library unloads them, and the next Develop visit loads them on its first photo", async () => {
    usedBefore();
    invoke.mockResolvedValue(null);
    photo("/s/a.NEF");
    const masking = renderHook(() => useAiMasking());
    const workspace = renderHook(({ develop }) => useAiModelLifecycle(develop), { initialProps: { develop: true } });
    await waitFor(() => expect(pathsOf("precompute_ai_foreground_mask")).toEqual(["/s/a.NEF"]), {
      timeout: AFTER_DELAY + 1000,
    });
    expect(calls("unload_ai_models")).toHaveLength(0);

    // The Library: the editor goes, the models are dropped.
    masking.unmount();
    workspace.rerender({ develop: false });
    expect(calls("unload_ai_models")).toEqual([["unload_ai_models", { reason: "library" }]]);

    // Back in Develop on another photo: loaded again with it.
    workspace.rerender({ develop: true });
    act(() => photo("/s/b.NEF"));
    renderHook(() => useAiMasking());
    await waitFor(() => expect(pathsOf("precompute_ai_foreground_mask")).toEqual(["/s/a.NEF", "/s/b.NEF"]), {
      timeout: AFTER_DELAY + 1000,
    });
    expect(calls("unload_ai_models")).toHaveLength(1);
  });

  it("starts loading them on entering Develop, before the photo is ready, only for earlier users", () => {
    invoke.mockResolvedValue(null);
    // Never used: nothing, also not at launch in the Library.
    const workspace = renderHook(({ develop }) => useAiModelLifecycle(develop), { initialProps: { develop: false } });
    workspace.rerender({ develop: true });
    expect(calls("preload_ai_mask_models")).toHaveLength(0);
    workspace.rerender({ develop: false });
    usedBefore();
    workspace.rerender({ develop: true });
    expect(calls("preload_ai_mask_models")).toHaveLength(1);
    // Library and back: loaded again.
    workspace.rerender({ develop: false });
    workspace.rerender({ develop: true });
    expect(calls("preload_ai_mask_models")).toHaveLength(2);
    expect(calls("unload_ai_models")).toHaveLength(2);
  });

  it("after an idle unload, opening photos does not load them again until masking is used", async () => {
    usedBefore();
    invoke.mockResolvedValue(null);
    renderHook(() => useAiModelLifecycle(true));
    await waitFor(() => expect(listeners.has(AI_MODELS_UNLOADED_EVENT)).toBe(true));
    act(() => listeners.get(AI_MODELS_UNLOADED_EVENT)!({ payload: "idle" }));
    photo("/s/a.NEF");
    renderHook(() => useAiMasking());
    await wait(AFTER_DELAY);
    expect(calls("precompute_ai_foreground_mask")).toHaveLength(0);
    // Masking use: computed ahead again.
    act(() => useUIStore.setState({ activePanel: Panel.Masks }));
    await waitFor(() => expect(pathsOf("precompute_ai_foreground_mask")).toEqual(["/s/a.NEF"]));
  });

  it("under memory pressure, recent masking use no longer loads them on the next photo", async () => {
    invoke.mockResolvedValue(null);
    renderHook(() => useAiModelLifecycle(true));
    photo("/s/a.NEF");
    renderHook(() => useAiMasking());
    act(() => useUIStore.setState({ activePanel: Panel.Masks }));
    await waitFor(() => expect(pathsOf("precompute_ai_sky_mask")).toEqual(["/s/a.NEF"]));
    act(() => useUIStore.setState({ activePanel: Panel.Adjustments }));
    await waitFor(() => expect(listeners.has(AI_MODELS_UNLOADED_EVENT)).toBe(true));
    act(() => listeners.get(AI_MODELS_UNLOADED_EVENT)!({ payload: "memoryPressure" }));
    act(() => photo("/s/b.NEF"));
    await wait(AFTER_DELAY);
    expect(pathsOf("precompute_ai_foreground_mask")).toEqual(["/s/a.NEF"]);
  });
});
