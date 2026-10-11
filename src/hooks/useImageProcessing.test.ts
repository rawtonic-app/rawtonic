import { act, cleanup, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { useImageProcessing } from "./useImageProcessing";
import { useEditorStore } from "../store/useEditorStore";
import { useUIStore } from "../store/useUIStore";
import { useSettingsStore } from "../store/useSettingsStore";
import { useLibraryStore } from "../store/useLibraryStore";
import { INITIAL_ADJUSTMENTS } from "../utils/adjustments";
import { Invokes, Panel } from "../components/ui/AppProperties";
import { debouncedSave } from "./useEditorActions";
import { toast } from "react-toastify";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("./useEditorActions", () => ({ debouncedSave: vi.fn(), registerScheduledRecipe: vi.fn(() => () => {}) }));
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
  });
  useUIStore.setState({ activeView: "editor" });
  useSettingsStore.setState({ appSettings: null });
  useLibraryStore.setState({ multiSelectedPaths: [] });
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
