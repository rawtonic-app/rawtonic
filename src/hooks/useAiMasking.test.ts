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
