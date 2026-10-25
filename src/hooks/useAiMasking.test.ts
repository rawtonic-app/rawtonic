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
  pendingObjectPrompts,
  resetAiMaskSession,
  useAiMasking,
  useAiModelLifecycle,
} from "./useAiMasking";
import { useEditorStore } from "../store/useEditorStore";
import { useProcessStore } from "../store/useProcessStore";
import { useUIStore } from "../store/useUIStore";
import { Panel } from "../components/ui/AppProperties";
import { INITIAL_ADJUSTMENTS } from "../utils/adjustments";
import { promptFrameKey } from "../utils/objectPrompts";
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

  it("leaves a pasted Object selection to be pointed at again, and ends its wait when it is", async () => {
    const object = layer("ai-subject", { startX: 600, startY: 400, endX: 1200, endY: 800, maskDataBase64: null, detectPending: true });
    useEditorStore.setState({
      selectedImage: { path: "/s/a.NEF", isReady: true, isRaw: true, width: 3000, height: 2000 } as never,
      adjustments: { ...structuredClone(INITIAL_ADJUSTMENTS), masks: [object] as never },
    });
    const { result } = renderHook(() => useAiMasking());
    await act(async () => {});
    expect(invoke.mock.calls.some((c) => c[0] === "generate_ai_subject_mask")).toBe(false);
    // The user points at the object again.
    invoke.mockResolvedValue({ maskDataBase64: "data:image/png;base64,BBBB", startX: 10, startY: 10, endX: 50, endY: 50 });
    await act(() => result.current.handleGenerateAiMask("sub-ai-subject", { x: 10, y: 10 }, { x: 50, y: 50 }));
    expect(subOf("sub-ai-subject")?.parameters).toMatchObject({ maskDataBase64: "data:image/png;base64,BBBB" });
    expect(subOf("sub-ai-subject")?.parameters).not.toHaveProperty("detectPending");
  });

  it("runs a second Object prompt pressed before the first is in after it: one history step each", async () => {
    const pending = deferredMasks();
    const object = layer("ai-subject", { maskDataBase64: null, grow: 0, feather: 0 });
    const initial = { ...structuredClone(INITIAL_ADJUSTMENTS), masks: [object] as never };
    useEditorStore.setState({
      selectedImage: { path: "/s/a.NEF", isReady: true, isRaw: true, width: 3000, height: 2000 } as never,
      adjustments: initial,
      history: [initial],
      historyIndex: 0,
    });
    const { result } = renderHook(() => useAiMasking());
    const a = { type: "point" as const, x: 10, y: 10 };
    const b = { type: "point" as const, x: 900, y: 500 };
    let first!: Promise<boolean>;
    let second!: Promise<boolean>;
    act(() => {
      first = result.current.handleGenerateAiMask("sub-ai-subject", a, a, [a]);
      second = result.current.handleGenerateAiMask("sub-ai-subject", a, a, [a, b]);
    });
    await waitFor(() => expect(pending).toHaveLength(1));
    const asked = () =>
      invoke.mock.calls.filter((c) => c[0] === "generate_ai_subject_mask").map((c) => c[1].prompts);
    // The second waits for the first.
    expect(asked()).toEqual([[a]]);
    await act(async () => {
      pending[0]({ maskDataBase64: "data:image/png;base64,AAAA" });
      await first;
    });
    await waitFor(() => expect(pending).toHaveLength(2));
    expect(asked()).toEqual([[a], [a, b]]);
    await act(async () => {
      pending[1]({ maskDataBase64: "data:image/png;base64,BBBB" });
      await second;
    });
    const promptsOf = (adjustments: { masks: { subMasks: { parameters: Record<string, unknown> }[] }[] }) =>
      adjustments.masks[0].subMasks[0].parameters.prompts;
    const { history } = useEditorStore.getState();
    expect(history.map(promptsOf)).toEqual([undefined, [a], [a, b]]);
    act(() => useEditorStore.getState().undo());
    expect(promptsOf(useEditorStore.getState().adjustments)).toEqual([a]);
    expect(subOf("sub-ai-subject")?.parameters.maskDataBase64).toBe("data:image/png;base64,AAAA");
  });

  it("drops a pending Object prompt when an undo changed the selection it was pressed on", async () => {
    const pending = deferredMasks();
    const a = { type: "point" as const, x: 10, y: 10 };
    const b = { type: "point" as const, x: 900, y: 500 };
    const empty = { ...structuredClone(INITIAL_ADJUSTMENTS), masks: [layer("ai-subject", { maskDataBase64: null })] as never };
    const withA = {
      ...structuredClone(INITIAL_ADJUSTMENTS),
      masks: [layer("ai-subject", { maskDataBase64: "data:image/png;base64,AAAA", prompts: [a] })] as never,
    };
    useEditorStore.setState({
      selectedImage: { path: "/s/a.NEF", isReady: true, isRaw: true, width: 3000, height: 2000 } as never,
      adjustments: withA,
      history: [empty, withA],
      historyIndex: 1,
    });
    const { result } = renderHook(() => useAiMasking());
    let added!: Promise<boolean>;
    act(() => {
      added = result.current.handleGenerateAiMask("sub-ai-subject", a, a, [a, b]);
    });
    await waitFor(() => expect(pending).toHaveLength(1));
    const frame = promptFrameKey(useEditorStore.getState().adjustments);
    expect(pendingObjectPrompts("sub-ai-subject", "/s/a.NEF", frame)).toEqual([a, b]);
    // A is undone before B is in.
    act(() => useEditorStore.getState().undo());
    expect(pendingObjectPrompts("sub-ai-subject", "/s/a.NEF", frame)).toBeUndefined();
    await act(async () => {
      pending[0]({ maskDataBase64: "data:image/png;base64,BBBB" });
      await added;
    });
    const state = useEditorStore.getState();
    expect(state.adjustments).toBe(empty);
    // The redo step is kept.
    expect(state.history).toEqual([empty, withA]);
    expect(state.historyIndex).toBe(0);
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

  it("has the backend place imported Lightroom geometry once, on the full decode", async () => {
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
    const pending = layer("radial", {
      lrNormalized: {
        top: 0.2,
        left: 0.3,
        bottom: 0.6,
        right: 0.7,
        angle: 0,
        feather: 50,
      },
    });
    // Shown on a preview proxy first, whose size may be rounded: renders
    // place the geometry on the full decode's size, so the editor waits.
    useEditorStore.setState({
      selectedImage: {
        path: "/s/a.NEF",
        isReady: true,
        isRaw: true,
        width: 5999,
        height: 4000,
      } as never,
      adjustments: {
        ...structuredClone(INITIAL_ADJUSTMENTS),
        masks: [pending] as never,
      },
    });
    const { rerender } = renderHook(() => useAiMasking());
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(invoke).not.toHaveBeenCalled();
    useEditorStore.setState((state) => ({
      selectedImage: { ...state.selectedImage, isDecoded: true, width: 6000, height: 4000 } as never,
    }));
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

  it("places Lightroom geometry in every history step, adding none: undo and redo keep it placed", async () => {
    const placedParameters = { centerX: 1, centerY: 2, radiusX: 3, radiusY: 4, rotation: 0, feather: 0.5 };
    invoke.mockImplementation(async (command: string, args: { adjustments: { masks: { subMasks: { id: string }[] }[] } }) =>
      command === "place_lightroom_masks"
        ? {
            masks: [
              {
                subMasks: args.adjustments.masks[0].subMasks.map((sub) => ({ ...sub, parameters: placedParameters })),
              },
            ],
          }
        : null,
    );
    const pending = layer("radial", { lrNormalized: { top: 0.2, left: 0.3, bottom: 0.6, right: 0.7 } });
    const initial = { ...structuredClone(INITIAL_ADJUSTMENTS), masks: [pending] as never };
    // An edit made on the preview proxy before the decode came in.
    const edited = { ...initial, exposure: 0.5 };
    useEditorStore.setState({
      selectedImage: { path: "/s/a.NEF", isReady: true, isDecoded: true, isRaw: true, width: 6000, height: 4000 } as never,
      adjustments: edited,
      history: [initial, edited],
      historyIndex: 1,
    });
    renderHook(() => useAiMasking());
    const subOf = (adjustments: { masks: { subMasks: { parameters: unknown }[] }[] }) =>
      adjustments.masks[0].subMasks[0].parameters;
    await waitFor(() => expect(subOf(useEditorStore.getState().adjustments)).toEqual(placedParameters));
    const state = useEditorStore.getState();
    expect(state.adjustments.exposure).toBe(0.5);
    expect(state.history).toHaveLength(2);
    expect(state.historyIndex).toBe(1);
    expect(state.history.map(subOf)).toEqual([placedParameters, placedParameters]);
    // Undo reaches the step before the edit with the geometry placed (the
    // editing tools need the placed form), and redo the edit.
    act(() => useEditorStore.getState().undo());
    expect(useEditorStore.getState().adjustments.exposure).toBe(0);
    expect(subOf(useEditorStore.getState().adjustments)).toEqual(placedParameters);
    act(() => useEditorStore.getState().redo());
    expect(useEditorStore.getState().adjustments.exposure).toBe(0.5);
    await new Promise((resolve) => setTimeout(resolve, 600));
    expect(useEditorStore.getState().history).toHaveLength(2);
    expect(invoke).toHaveBeenCalledTimes(1);
  });

  it("asks again for Lightroom geometry whose placement came back for another photo", async () => {
    let answer: (value: unknown) => void = () => {};
    invoke.mockImplementation(
      (command: string, args: { adjustments: { masks: { subMasks: { id: string }[] }[] } }) =>
        command === "place_lightroom_masks"
          ? new Promise((resolve) => {
              answer = () =>
                resolve({
                  masks: [
                    {
                      subMasks: args.adjustments.masks[0].subMasks.map((sub) => ({
                        ...sub,
                        parameters: { centerX: 1, centerY: 2, radiusX: 3, radiusY: 4, rotation: 0, feather: 0.5 },
                      })),
                    },
                  ],
                });
            })
          : Promise.resolve(null),
    );
    const pending = layer("radial", { lrNormalized: { top: 0.2, left: 0.3, bottom: 0.6, right: 0.7 } });
    const photo = (path: string) =>
      ({ path, isReady: true, isDecoded: true, isRaw: true, width: 6000, height: 4000 }) as never;
    useEditorStore.setState({
      selectedImage: photo("/s/a.NEF"),
      adjustments: { ...structuredClone(INITIAL_ADJUSTMENTS), masks: [pending] as never },
    });
    renderHook(() => useAiMasking());
    await waitFor(() => expect(invoke).toHaveBeenCalledTimes(1));
    // A → B while A's placement is under way, then back to A.
    useEditorStore.setState({
      selectedImage: photo("/s/b.NEF"),
      adjustments: { ...structuredClone(INITIAL_ADJUSTMENTS), masks: [] as never },
    });
    answer(null);
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(invoke).toHaveBeenCalledTimes(1);
    useEditorStore.setState({
      selectedImage: { ...(photo("/s/a.NEF") as object), isDecoded: false } as never,
      adjustments: { ...structuredClone(INITIAL_ADJUSTMENTS), masks: [pending] as never },
    });
    useEditorStore.setState({ selectedImage: photo("/s/a.NEF") });
    await waitFor(() => expect(invoke).toHaveBeenCalledTimes(2));
    expect(invoke.mock.calls[1][1]).toMatchObject({ path: "/s/a.NEF" });
  });

  const pendingSpot = { mode: "heal", opacity: 100, feather: 0, lrNormalized: { x: 0.25, y: 0.5, radius: 0.01 } };
  const ownSpot = { id: "own", mode: "clone", x: 10, y: 10, radius: 5, sourceX: 30, sourceY: 10 };
  const pendingEye = { kind: "red", lrNormalized: { x: 0.5, y: 0.5, width: 0.01, height: 0.01 } };
  const placeRetouch = async (_command: string, args: { jsAdjustments: { spots: object[]; redEyes: object[] } }) => ({
    spots: args.jsAdjustments.spots.map((spot) =>
      "lrNormalized" in spot ? { id: "placed-spot", mode: "heal", x: 1500, y: 2000, radius: 60 } : spot,
    ),
    redEyes: args.jsAdjustments.redEyes.map(() => ({ id: "placed-eye", kind: "red", x: 0.5, y: 0.33 })),
  });

  it("has the backend place imported Lightroom spots and red eyes in the open photo, where they stand", async () => {
    invoke.mockImplementation(async (command: string, args: never) =>
      command === "place_lightroom_retouch" ? placeRetouch(command, args) : null,
    );
    useEditorStore.setState({
      selectedImage: { path: "/s/a.NEF", isReady: true, isDecoded: true, isRaw: true, width: 6000, height: 4000 } as never,
      adjustments: {
        ...structuredClone(INITIAL_ADJUSTMENTS),
        spots: [pendingSpot, ownSpot] as never,
        redEyes: [pendingEye] as never,
      },
    });
    const { rerender } = renderHook(() => useAiMasking());
    await waitFor(() => expect(useEditorStore.getState().adjustments.spots?.[0]).toMatchObject({ id: "placed-spot" }));
    const { spots, redEyes } = useEditorStore.getState().adjustments;
    expect(spots?.[1]).toEqual(ownSpot);
    expect(redEyes?.[0]).toMatchObject({ id: "placed-eye" });
    expect(invoke).toHaveBeenCalledTimes(1);
    expect(invoke.mock.calls[0][1]).toMatchObject({ path: "/s/a.NEF" });
    rerender();
    expect(invoke).toHaveBeenCalledTimes(1);
  });

  /** Placement answers held until the test gives them, in order of asking. */
  const heldPlacements = () => {
    const answers: ((value: unknown) => void)[] = [];
    invoke.mockImplementation(
      (command: string) =>
        new Promise((resolve) => {
          if (command === "place_lightroom_retouch") answers.push(resolve);
          else resolve(null);
        }),
    );
    return answers;
  };
  const openWithPendingSpot = (path: string, isDecoded = true) =>
    useEditorStore.setState({
      selectedImage: { path, isReady: true, isDecoded, isRaw: true, width: 6000, height: 4000 } as never,
      adjustments: { ...structuredClone(INITIAL_ADJUSTMENTS), spots: [pendingSpot] as never },
    });
  const placeCalls = () => invoke.mock.calls.filter(([command]) => command === "place_lightroom_retouch");

  it("asks again for a placement dropped because another photo opened, even when A is open again first", async () => {
    const answers = heldPlacements();
    openWithPendingSpot("/s/a.NEF");
    const { rerender } = renderHook(() => useAiMasking());
    await waitFor(() => expect(placeCalls()).toHaveLength(1));
    // B opens, then A again, all before A's first answer: the backend had B
    // loaded and says so only now.
    useEditorStore.setState({ selectedImage: { path: "/s/b.NEF", isReady: false } as never });
    rerender();
    openWithPendingSpot("/s/a.NEF", false);
    rerender();
    openWithPendingSpot("/s/a.NEF");
    rerender();
    expect(placeCalls()).toHaveLength(1);
    await act(async () => answers[0](null));
    await waitFor(() => expect(placeCalls()).toHaveLength(2));
    await act(async () => answers[1](await placeRetouch("", { jsAdjustments: { spots: [pendingSpot], redEyes: [] } })));
    await waitFor(() => expect(useEditorStore.getState().adjustments.spots?.[0]).toMatchObject({ id: "placed-spot" }));
  });

  it("drops a placement made for a geometry the photo no longer has and asks again for the current one", async () => {
    const answers = heldPlacements();
    openWithPendingSpot("/s/a.NEF");
    renderHook(() => useAiMasking());
    await waitFor(() => expect(placeCalls()).toHaveLength(1));
    // Turned a quarter while the placement ran.
    act(() => {
      useEditorStore.setState((state) => ({ adjustments: { ...state.adjustments, orientationSteps: 1 } }));
    });
    await act(async () => answers[0](await placeRetouch("", { jsAdjustments: { spots: [pendingSpot], redEyes: [] } })));
    expect(useEditorStore.getState().adjustments.spots?.[0]).toEqual(pendingSpot);
    await waitFor(() => expect(placeCalls()).toHaveLength(2));
    expect(placeCalls()[1][1]).toMatchObject({ jsAdjustments: { orientationSteps: 1 } });
    await act(async () => answers[1](await placeRetouch("", { jsAdjustments: { spots: [pendingSpot], redEyes: [] } })));
    await waitFor(() => expect(useEditorStore.getState().adjustments.spots?.[0]).toMatchObject({ id: "placed-spot" }));
  });

  it("places pending items again when an undo brings them back", async () => {
    invoke.mockImplementation(async (command: string, args: never) =>
      command === "place_lightroom_retouch" ? placeRetouch(command, args) : null,
    );
    openWithPendingSpot("/s/a.NEF");
    renderHook(() => useAiMasking());
    await waitFor(() => expect(useEditorStore.getState().adjustments.spots?.[0]).toMatchObject({ id: "placed-spot" }));
    // An undo restores the recipe from before the placement.
    act(() => {
      useEditorStore.setState((state) => ({ adjustments: { ...state.adjustments, spots: [pendingSpot] as never } }));
    });
    await waitFor(() => expect(placeCalls()).toHaveLength(2));
    await waitFor(() => expect(useEditorStore.getState().adjustments.spots?.[0]).toMatchObject({ id: "placed-spot" }));
  });

  it("asks again for items the geometry could not show once the geometry changes", async () => {
    // The backend keeps them pending (nothing it could show).
    invoke.mockImplementation(async (command: string, args: { jsAdjustments: { spots: object[]; redEyes: object[] } }) =>
      command === "place_lightroom_retouch" ? { spots: args.jsAdjustments.spots, redEyes: args.jsAdjustments.redEyes } : null,
    );
    openWithPendingSpot("/s/a.NEF");
    const { rerender } = renderHook(() => useAiMasking());
    await waitFor(() => expect(placeCalls()).toHaveLength(1));
    rerender();
    await act(async () => {});
    expect(placeCalls()).toHaveLength(1);
    expect(useEditorStore.getState().adjustments.spots?.[0]).toEqual(pendingSpot);
    act(() => {
      useEditorStore.setState((state) => ({ adjustments: { ...state.adjustments, transformDistortion: 10 } }));
    });
    await waitFor(() => expect(placeCalls()).toHaveLength(2));
  });

  it("places on the full decode: not from the preview proxy, at once when the decode is in", async () => {
    invoke.mockImplementation(async (command: string, args: never) =>
      command === "place_lightroom_retouch" ? placeRetouch(command, args) : null,
    );
    openWithPendingSpot("/s/a.NEF", false);
    renderHook(() => useAiMasking());
    await act(async () => {});
    expect(placeCalls()).toHaveLength(0);
    act(() => {
      useEditorStore.setState((state) => ({ selectedImage: { ...state.selectedImage!, isDecoded: true } }));
    });
    await waitFor(() => expect(useEditorStore.getState().adjustments.spots?.[0]).toMatchObject({ id: "placed-spot" }));
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
