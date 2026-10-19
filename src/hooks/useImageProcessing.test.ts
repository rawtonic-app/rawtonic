import { act, cleanup, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { useImageProcessing } from "./useImageProcessing";
import { useEditorStore } from "../store/useEditorStore";
import { useUIStore } from "../store/useUIStore";
import { useSettingsStore } from "../store/useSettingsStore";
import { useLibraryStore } from "../store/useLibraryStore";
import { INITIAL_ADJUSTMENTS, INITIAL_MASK_CONTAINER } from "../utils/adjustments";
import { Invokes, Panel } from "../components/ui/AppProperties";
import { debouncedSave } from "./useEditorActions";
import { toast } from "react-toastify";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const { applyAdjustmentsToPaths } = vi.hoisted(() => ({
  applyAdjustmentsToPaths: vi.fn((_partial: unknown, _paths?: string[]) => Promise.resolve()),
}));
vi.mock("./useEditorActions", () => ({
  debouncedSave: vi.fn(),
  registerScheduledRecipe: vi.fn(() => () => {}),
  useEditorActions: () => ({ applyAdjustmentsToPaths }),
}));
vi.mock("react-toastify", () => ({ toast: { error: vi.fn() } }));

const photo = (path: string, isReady = true) => ({
  path,
  isReady,
  isRaw: true,
  width: 4000,
  height: 3000,
  exif: {},
  thumbnailUrl: "",
});
let complete: ((value: ArrayBuffer) => void)[];
let fail: ((reason: unknown) => void)[];
const calls = () =>
  vi
    .mocked(invoke)
    .mock.calls.filter(([name]) => name === Invokes.ApplyAdjustments);
const args = (index: number) =>
  calls()[index][1] as { sequence: number; jsAdjustments: { exposure: number } };
const finish = async () => {
  await act(async () => {
    complete.shift()!(new ArrayBuffer(0));
  });
};
const mount = () => {
  const refs = {
    previewJobIdRef: { current: 0 },
    latestRenderedJobIdRef: { current: 0 },
    currentResRef: { current: 1920 },
  };
  const previous = { current: null };
  return renderHook(() =>
    useImageProcessing({ current: null }, previous, refs),
  );
};

beforeEach(() => {
  vi.useFakeTimers();
  complete = [];
  fail = [];
  vi.mocked(invoke).mockImplementation(
    () =>
      new Promise((resolve, reject) => {
        complete.push(resolve);
        fail.push(reject);
      }),
  );
  useEditorStore.setState({
    selectedImage: photo("/first.dng"),
    adjustments: structuredClone(INITIAL_ADJUSTMENTS),
    previewOverride: null,
    isSliderDragging: false,
    isWaveformVisible: false,
    displaySize: { width: 0, height: 0 },
    originalSize: { width: 4000, height: 3000 },
    interactivePatch: null,
    patchesSentToBackend: new Set(),
    clippingWarnings: false,
    softProof: false,
    softProofSpace: "srgb",
    softProofWarning: false,
    colorRangeShow: null,
    layerSolo: null,
    nativeMaskOverlay: null,
  });
  useUIStore.setState({ activeView: "editor" });
  useSettingsStore.setState({ appSettings: null });
  useLibraryStore.setState({ multiSelectedPaths: [] });
});

it("rerenders browser previews when clipping and soft proof change without saving the recipe", async () => {
  mount();
  await finish(); // Browser response, not WGPU_RENDER.
  const recipe = useEditorStore.getState().adjustments;
  act(() => useEditorStore.setState({ clippingWarnings: true }));
  expect(calls()).toHaveLength(2);
  expect(calls()[1][1]).toMatchObject({ jsAdjustments: { showClipping: true }, isInteractive: false });
  await finish();
  act(() => useEditorStore.setState({ softProof: true, softProofSpace: "adobeRgb", softProofWarning: true }));
  expect(calls()).toHaveLength(3);
  expect(calls()[2][1]).toMatchObject({ jsAdjustments: { softProof: "adobeRgb", softProofWarning: true } });
  await finish();
  act(() => useEditorStore.setState({ softProof: false, clippingWarnings: false, colorRangeShow: "red" }));
  expect(calls()).toHaveLength(4);
  expect(calls()[3][1]).toMatchObject({ jsAdjustments: { showClipping: false, colorRangeShow: "red" } });
  expect(useEditorStore.getState().adjustments).toBe(recipe);
  expect(debouncedSave).not.toHaveBeenCalled();
});

it("renders a layer solo without changing or saving the recipe", async () => {
  const layer = (id: string) => ({ ...structuredClone(INITIAL_MASK_CONTAINER), id, name: id, subMasks: [] });
  useEditorStore.setState({ adjustments: { ...structuredClone(INITIAL_ADJUSTMENTS), masks: [layer("a"), layer("b")] } });
  mount();
  await finish();
  const recipe = useEditorStore.getState().adjustments;
  act(() => useEditorStore.setState({ layerSolo: { layerId: "b", componentId: null } }));
  expect(calls()).toHaveLength(2);
  const sent = (calls()[1][1] as { jsAdjustments: { masks: { id: string; visible: boolean }[] } }).jsAdjustments;
  expect(sent.masks.map((m) => [m.id, m.visible])).toEqual([["a", false], ["b", true]]);
  await finish();
  act(() => useEditorStore.setState({ layerSolo: null }));
  expect(calls()).toHaveLength(3);
  const shown = (calls()[2][1] as { jsAdjustments: { masks: { visible: boolean }[] } }).jsAdjustments;
  expect(shown.masks.map((m) => m.visible)).toEqual([true, true]);
  expect(useEditorStore.getState().adjustments).toBe(recipe);
  expect(recipe.masks.every((m) => m.visible)).toBe(true);
  expect(debouncedSave).not.toHaveBeenCalled();
});

it("does not reencode a browser preview for native-only mask overlay changes", async () => {
  mount();
  await finish();
  act(() => useEditorStore.setState({ nativeMaskOverlay: { maskId: "layer", opacity: 100, color: [1, 0, 0], grayscale: false } }));
  expect(calls()).toHaveLength(1);
});
afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.clearAllMocks();
});

it("renders immediately and retains the released slider value under a slow native render", async () => {
  mount();
  expect(calls()).toHaveLength(1); // No initial 50-ms timer.
  await finish();
  act(() =>
    useEditorStore.setState({
      isSliderDragging: true,
      adjustments: { ...INITIAL_ADJUSTMENTS, exposure: 1 },
    }),
  );
  act(() =>
    useEditorStore.setState({
      adjustments: { ...INITIAL_ADJUSTMENTS, exposure: 2 },
    }),
  );
  act(() =>
    useEditorStore.setState({
      isSliderDragging: false,
      adjustments: { ...INITIAL_ADJUSTMENTS, exposure: 3 },
    }),
  );
  // Sent at once; the native worker keeps submission order by sequence.
  expect(calls()).toHaveLength(4);
  const sequences = [0, 1, 2, 3].map((index) => args(index).sequence);
  expect(sequences.map((sequence) => sequence - sequences[0])).toEqual([0, 1, 2, 3]);
  expect(sequences[0]).toBeGreaterThan(Date.UTC(2026, 0) * 1000);
  expect(calls()[3][1]).toMatchObject({
    isInteractive: false,
    jsAdjustments: { exposure: 3 },
    requestAnalytics: true,
  });
  expect(debouncedSave).toHaveBeenCalledExactlyOnceWith(
    "/first.dng",
    expect.objectContaining({ exposure: 3 }),
  );
  await finish();
  await finish();
  await finish();
  act(() => vi.advanceTimersByTime(1000));
  expect(calls()).toHaveLength(4);
});

it("discards queued adjustments from the previous photograph while waiting for the new source", async () => {
  mount();
  act(() =>
    useEditorStore.setState({
      isSliderDragging: true,
      adjustments: { ...INITIAL_ADJUSTMENTS, exposure: 4 },
    }),
  );
  act(() =>
    useEditorStore.setState({
      adjustments: { ...INITIAL_ADJUSTMENTS, exposure: 5 },
    }),
  );
  act(() =>
    useEditorStore.setState({
      adjustments: { ...INITIAL_ADJUSTMENTS, exposure: 6 },
    }),
  );
  // All four render slots are taken; exposure 7 stays pending.
  act(() =>
    useEditorStore.setState({
      adjustments: { ...INITIAL_ADJUSTMENTS, exposure: 7 },
    }),
  );
  expect(calls()).toHaveLength(4);
  act(() =>
    useEditorStore.setState({
      selectedImage: photo("/second.dng", false),
      isSliderDragging: false,
      adjustments: { ...INITIAL_ADJUSTMENTS, exposure: -1 },
    }),
  );
  for (let index = 0; index < 4; index++) await finish();
  expect(calls()).toHaveLength(4);
  act(() => useEditorStore.setState({ selectedImage: photo("/second.dng") }));
  expect(calls()).toHaveLength(5);
  expect(calls()[4][1]).toMatchObject({
    isInteractive: false,
    jsAdjustments: { exposure: -1 },
  });
  expect([0, 1, 2, 3, 4].some((index) => args(index).jsAdjustments.exposure === 7)).toBe(false);
  expect(debouncedSave).not.toHaveBeenCalled();
  await finish();
});

it("keeps original-preview overrides out of saved adjustments", async () => {
  mount();
  await finish();
  act(() =>
    useEditorStore.setState({
      previewOverride: { ...INITIAL_ADJUSTMENTS, exposure: -2 },
    }),
  );
  expect(calls()[1][1]).toMatchObject({ jsAdjustments: { exposure: -2 } });
  expect(debouncedSave).not.toHaveBeenCalled();
  await finish();
});

it("takes a batch's reloaded recipe as the sync baseline, so the next edit syncs only itself", async () => {
  useSettingsStore.setState({
    appSettings: { copyPasteSettings: { autoSync: true, includedAdjustments: ["exposure", "rotation", "crop"] } } as never,
  });
  useLibraryStore.setState({ multiSelectedPaths: ["/first.dng", "/second.dng"] });
  mount();
  await finish();
  // A batch holds the open photo and reloads it with its own result.
  act(() => useEditorStore.setState({ heldRecipePaths: ["/first.dng"] }));
  act(() =>
    useEditorStore.setState({
      adjustments: { ...useEditorStore.getState().adjustments, rotation: -2.5, crop: { unit: "px", x: 10, y: 8, width: 380, height: 250 } },
    }),
  );
  await finish();
  act(() => useEditorStore.setState({ heldRecipePaths: [] }));
  vi.mocked(invoke).mockClear();
  applyAdjustmentsToPaths.mockClear();
  act(() => useEditorStore.setState({ adjustments: { ...useEditorStore.getState().adjustments, exposure: 0.5 } }));
  const synced = applyAdjustmentsToPaths.mock.calls;
  expect(synced).toHaveLength(1);
  expect(synced[0][1]).toEqual(["/second.dng"]);
  const delta = synced[0][0] as Record<string, unknown>;
  expect(delta.exposure).toBe(0.5);
  expect(delta).not.toHaveProperty("rotation");
  expect(delta).not.toHaveProperty("crop");
  await finish();
});

it("takes a batch's result as the sync baseline also when it arrives during a before preview", async () => {
  useSettingsStore.setState({
    appSettings: { copyPasteSettings: { autoSync: true, includedAdjustments: ["exposure", "rotation", "crop"] } } as never,
  });
  useLibraryStore.setState({ multiSelectedPaths: ["/first.dng", "/second.dng"] });
  mount();
  await finish();
  const before = useEditorStore.getState().adjustments;
  act(() => useEditorStore.setState({ previewOverride: { ...before, exposure: -2 }, heldRecipePaths: ["/first.dng"] }));
  act(() =>
    useEditorStore.setState({
      adjustments: { ...before, rotation: -2.5, crop: { unit: "px", x: 10, y: 8, width: 380, height: 250 } },
    }),
  );
  act(() => useEditorStore.setState({ heldRecipePaths: [] }));
  vi.mocked(invoke).mockClear();
  applyAdjustmentsToPaths.mockClear();
  act(() => useEditorStore.setState({ previewOverride: null }));
  expect(applyAdjustmentsToPaths).not.toHaveBeenCalled();
  while (complete.length) await finish();
});

it("reports a native render failure but not a superseded preview", async () => {
  mount();
  await act(async () => {
    fail.shift()!("Superseded or worker failed");
  });
  expect(toast.error).not.toHaveBeenCalled();
  act(() =>
    useEditorStore.setState({
      adjustments: { ...INITIAL_ADJUSTMENTS, exposure: 1 },
    }),
  );
  const error =
    "Preview rendering failed: GPU shader 'Image Processing Shader' could not be created";
  await act(async () => {
    fail.shift()!(error);
  });
  expect(toast.error).toHaveBeenCalledExactlyOnceWith(error, {
    toastId: "Preview rendering failed",
  });
});

it("renders the uncropped, unrotated crop-editing frame natively while cropping", async () => {
  const wgpuReply = () => new TextEncoder().encode("WGPU_RENDER").buffer as ArrayBuffer;
  useEditorStore.setState({
    hasRenderedFirstFrame: true,
    nativeCropFrame: false,
    adjustments: {
      ...structuredClone(INITIAL_ADJUSTMENTS),
      rotation: 3,
      crop: { x: 10, y: 10, width: 300, height: 200, unit: "px" },
    },
  });
  useUIStore.setState({ activePanel: Panel.Adjustments });
  mount();
  expect(calls()[0][1]).toMatchObject({ cropEdit: false, jsAdjustments: { rotation: 3 } });
  await act(async () => complete.shift()!(wgpuReply()));
  act(() => useUIStore.setState({ activePanel: Panel.Crop }));
  expect(calls()).toHaveLength(2);
  expect(calls()[1][1]).toMatchObject({
    cropEdit: true,
    roi: null,
    maskOverlay: null,
    jsAdjustments: { rotation: 0, crop: null },
  });
  // The CPU crop preview is not requested with the native display.
  expect(vi.mocked(invoke).mock.calls.some(([name]) => name === Invokes.GenerateUncroppedPreview)).toBe(false);
  await act(async () => complete.shift()!(wgpuReply()));
  expect(useEditorStore.getState().nativeCropFrame).toBe(true);
  act(() => useUIStore.setState({ activePanel: Panel.Adjustments }));
  expect(calls()[2][1]).toMatchObject({ cropEdit: false, jsAdjustments: { rotation: 3 } });
  await act(async () => complete.shift()!(wgpuReply()));
  expect(useEditorStore.getState().nativeCropFrame).toBe(false);
});

it("asks for the split view's before after finished native frames, one request at a time", async () => {
  const nativeFrame = () => new TextEncoder().encode("WGPU_RENDER").buffer as ArrayBuffer;
  const befores = () =>
    vi.mocked(invoke).mock.calls.filter(([name]) => name === Invokes.RenderSplitBefore);
  useEditorStore.setState({
    splitView: true,
    hasRenderedFirstFrame: true,
    clippingWarnings: true,
    adjustments: { ...structuredClone(INITIAL_ADJUSTMENTS), exposure: 2, rotation: 3 },
  });
  useUIStore.setState({ activePanel: Panel.Adjustments });
  mount();
  expect(befores()).toHaveLength(0);
  await act(async () => complete.shift()!(nativeFrame()));
  expect(befores()).toHaveLength(1);
  // The unedited recipe with the edit's framing and the view states.
  expect(befores()[0][1]).toMatchObject({
    path: "/first.dng",
    targetResolution: 1920,
    jsAdjustments: { exposure: INITIAL_ADJUSTMENTS.exposure, rotation: 3, showClipping: true },
  });

  // A frame finished while the before renders: one more request, afterwards.
  act(() =>
    useEditorStore.setState({
      adjustments: { ...structuredClone(INITIAL_ADJUSTMENTS), exposure: 4, rotation: 3 },
    }),
  );
  await act(async () => complete.splice(1, 1)[0](nativeFrame()));
  expect(befores()).toHaveLength(1);
  await act(async () => (complete.shift() as unknown as (value: boolean) => void)(true));
  expect(befores()).toHaveLength(2);
  await act(async () => (complete.shift() as unknown as (value: boolean) => void)(true));

  // Without the split, frames ask for nothing.
  act(() => useEditorStore.setState({ splitView: false }));
  act(() =>
    useEditorStore.setState({
      adjustments: { ...structuredClone(INITIAL_ADJUSTMENTS), exposure: 5 },
    }),
  );
  await act(async () => complete.shift()!(nativeFrame()));
  expect(befores()).toHaveLength(2);
  useEditorStore.setState({ clippingWarnings: false, hasRenderedFirstFrame: false });
});

it("renders the split view's before again when a view state changes on live frames", async () => {
  const nativeFrame = () => new TextEncoder().encode("WGPU_RENDER").buffer as ArrayBuffer;
  const befores = () =>
    vi.mocked(invoke).mock.calls.filter(([name]) => name === Invokes.RenderSplitBefore);
  useEditorStore.setState({ splitView: true, hasRenderedFirstFrame: true });
  useUIStore.setState({ activePanel: Panel.Adjustments });
  mount();
  await act(async () => complete.shift()!(nativeFrame()));
  expect(befores()).toHaveLength(1);
  await act(async () => (complete.shift() as unknown as (value: boolean) => void)(true));
  act(() => useEditorStore.setState({ clippingWarnings: true }));
  expect(befores()).toHaveLength(2);
  expect(befores()[1][1]).toMatchObject({ jsAdjustments: { showClipping: true } });
  await act(async () => (complete.pop() as unknown as (value: boolean) => void)(true));
  act(() => useEditorStore.setState({ softProof: true, softProofSpace: "adobeRgb" }));
  expect(befores()).toHaveLength(3);
  expect(befores()[2][1]).toMatchObject({ jsAdjustments: { softProof: "adobeRgb" } });
  await act(async () => (complete.pop() as unknown as (value: boolean) => void)(true));
  useEditorStore.setState({ splitView: false, hasRenderedFirstFrame: false });
});

describe("AI Denoise busy note", () => {
  const decoded = (path: string) => ({ ...photo(path), isDecoded: true });
  /** Starts a develop request for AI Denoise and returns how to end it. */
  const startDenoise = () => {
    useEditorStore.setState({
      selectedImage: decoded("/first.dng"),
      adjustments: { ...structuredClone(INITIAL_ADJUSTMENTS), aiDenoise: 50 },
      aiDenoiseBusy: false,
    });
    mount();
    const index = vi.mocked(invoke).mock.calls.findIndex(([name]) => name === Invokes.ApplyAiDenoise);
    expect(index).toBeGreaterThanOrEqual(0);
    expect(useEditorStore.getState().aiDenoiseBusy).toBe(true);
    return { resolve: complete[index] as unknown as (value: number) => void, reject: fail[index] };
  };

  it("clears when the request ends after another photograph opened", async () => {
    const request = startDenoise();
    act(() => useEditorStore.setState({ selectedImage: photo("/second.dng", false) }));
    await act(async () => request.resolve(50));
    expect(useEditorStore.getState().aiDenoiseBusy).toBe(false);
  });

  it("clears when the request is cancelled", async () => {
    const request = startDenoise();
    await act(async () => request.reject("Load cancelled"));
    expect(useEditorStore.getState().aiDenoiseBusy).toBe(false);
    expect(toast.error).not.toHaveBeenCalled();
  });

  it("stays while a newer request runs", async () => {
    const first = startDenoise();
    act(() =>
      useEditorStore.setState({ adjustments: { ...structuredClone(INITIAL_ADJUSTMENTS), aiDenoise: 70 } }),
    );
    await act(async () => first.reject("Load cancelled"));
    expect(useEditorStore.getState().aiDenoiseBusy).toBe(true);
  });
});
