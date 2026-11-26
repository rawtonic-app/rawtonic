import { act, cleanup, renderHook } from '@testing-library/react';
import { resetActivityBridges, startActivityBridges } from '../activity/bridges';
import { resetActivityStore, useActivityStore } from '../store/useActivityStore';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { DEVELOP_DRAFT_EVENT, PROXY_FRAME_FIRST_MS, useImageLoader, waitForFirstFrame } from './useImageLoader';
import { useImageProcessing } from './useImageProcessing';
import { useEditorStore } from '../store/useEditorStore';
import { useUIStore } from '../store/useUIStore';
import { useSettingsStore } from '../store/useSettingsStore';
import { useLibraryStore } from '../store/useLibraryStore';
import {
  AUTO_CA_VERSION,
  CAMERA_CA_VERSION,
  CAMERA_GEOMETRY_VERSION,
  FRINGE_VERSION,
  INITIAL_ADJUSTMENTS
} from '../utils/adjustments';
import { Invokes } from '../components/ui/AppProperties';
import { debouncedSave, updateAdjustments, waitForPendingMetadataSave } from './useEditorActions';
import { notify as toast } from '../activity/notify';
import { adoptQueuedSelections } from '../utils/queuedSelections';
import { historyOrders, nextUndoOrder, setHistoryOrders } from '../utils/undoOrder';
import { editorRecipe } from '../utils/neverEdited';
import { runExportJobs } from '../utils/runExportJobs';
import { DEFAULT_RECIPE_DRAFT, exportSettingsFromDraft } from '../utils/exportRecipes';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn() }));
vi.mock('../activity/notify', () => ({ notify: { error: vi.fn(), success: vi.fn() } }));

type Pending = {
  command: string;
  path: string;
  resolve: (value: unknown) => void;
  reject: (reason: unknown) => void;
  settled?: boolean;
};
let pending: Pending[];
let proxyResult: { width: number; height: number; isRaw: boolean } | null;
let emitDraft: ((payload: { path: string; width: number; height: number; isRaw: boolean }) => void) | null;
const photo = (path: string) => ({
  path,
  isReady: false,
  isRaw: true,
  width: 4000,
  height: 3000,
  exif: {},
  thumbnailUrl: ''
});
const nativeResult = {
  width: 4000,
  height: 3000,
  is_raw: true,
  exif: {},
  metadata: {}
};
const resolve = async (command: string, path: string, value: unknown) => {
  // The camera JPEG is asked for once the stored-proxy answer is in.
  await act(async () => {});
  const request = pending.find((request) => !request.settled && request.command === command && request.path === path)!;
  request.settled = true;
  await act(async () => request.resolve(value));
};

beforeEach(() => {
  pending = [];
  proxyResult = null;
  emitDraft = null;
  vi.mocked(listen).mockImplementation((event, handler) => {
    if (event === DEVELOP_DRAFT_EVENT) {
      emitDraft = (payload) => (handler as (e: { payload: typeof payload }) => void)({ payload });
    }
    return Promise.resolve(() => {});
  });
  vi.stubGlobal('URL', { createObjectURL: vi.fn(() => 'blob:priority-preview'), revokeObjectURL: vi.fn() });
  vi.mocked(invoke).mockImplementation((command, args) => {
    if (command === Invokes.ApplyAdjustments) return Promise.resolve(new ArrayBuffer(0));
    // DNGs carry no camera lens data (Fujifilm/Sony only).
    if (command === Invokes.GetCameraLensProfile) return Promise.resolve(null);
    // No preview proxy unless a test provides one.
    if (command === Invokes.LoadImageProxy) return Promise.resolve(proxyResult);
    return new Promise((resolve, reject) =>
      pending.push({ command, path: (args as { path: string }).path, resolve, reject })
    );
  });
  useEditorStore.setState({
    selectedImage: photo('/first.dng'),
    adjustments: { ...INITIAL_ADJUSTMENTS, exposure: 4 },
    previewOverride: null,
    isSliderDragging: false,
    isWaveformVisible: false,
    displaySize: { width: 0, height: 0 },
    originalSize: { width: 0, height: 0 },
    finalPreview: null,
    hasRenderedFirstFrame: false,
    interactivePatch: null,
    patchesSentToBackend: new Set()
  });
  useUIStore.setState({ activeView: 'editor' });
  useSettingsStore.setState({ appSettings: null });
  useLibraryStore.setState({ multiSelectedPaths: [], isViewLoading: true });
});

afterEach(() => {
  debouncedSave.cancel();
  cleanup();
  vi.clearAllMocks();
  vi.unstubAllGlobals();
});

it('loads source and metadata concurrently, then renders one complete initial state', async () => {
  const previous = { current: null };
  const renderRefs = {
    previewJobIdRef: { current: 0 },
    latestRenderedJobIdRef: { current: 0 },
    currentResRef: { current: 0 }
  };
  renderHook(() => {
    useImageLoader({ current: null });
    useImageProcessing({ current: null }, previous, renderRefs);
  });
  await act(async () => {});
  // Without a stored proxy the decode starts as soon as that is known.
  expect(pending.map(({ command }) => command)).toEqual([
    Invokes.LoadMetadata,
    Invokes.LoadEmbeddedPreview,
    Invokes.LoadImage
  ]);
  await resolve(Invokes.LoadImage, '/first.dng', nativeResult);
  expect(useEditorStore.getState().selectedImage?.isReady).toBe(false);
  expect(vi.mocked(invoke).mock.calls.filter(([command]) => command === Invokes.ApplyAdjustments)).toHaveLength(0);
  await resolve(Invokes.LoadMetadata, '/first.dng', { adjustments: { ...INITIAL_ADJUSTMENTS, exposure: 1.5 } });
  const state = useEditorStore.getState();
  expect(state.selectedImage?.isReady).toBe(true);
  expect(state.originalSize).toEqual({ width: 4000, height: 3000 });
  expect(state.adjustments).toMatchObject({ exposure: 1.5, aspectRatio: 4 / 3 });
  expect(state.history).toEqual([state.adjustments]);
  const previews = vi.mocked(invoke).mock.calls.filter(([command]) => command === Invokes.ApplyAdjustments);
  expect(previews).toHaveLength(1);
  expect(previews[0][1]).toMatchObject({ jsAdjustments: { exposure: 1.5, aspectRatio: 4 / 3 } });
  expect(
    vi.mocked(invoke).mock.calls.filter(([command]) => command === Invokes.SaveMetadataAndUpdateThumbnail)
  ).toHaveLength(0);
});

it('ignores a source and metadata pair completed after selecting another photograph', async () => {
  renderHook(() => useImageLoader({ current: null }));
  // The first decode is under way when the next photograph is selected.
  await act(async () => {});
  act(() => useEditorStore.setState({ selectedImage: photo('/second.dng') }));
  await resolve(Invokes.LoadMetadata, '/first.dng', { adjustments: { ...INITIAL_ADJUSTMENTS, exposure: 3 } });
  await resolve(Invokes.LoadImage, '/first.dng', nativeResult);
  expect(useEditorStore.getState().selectedImage).toMatchObject({ path: '/second.dng', isReady: false });
  await resolve(Invokes.LoadMetadata, '/second.dng', { adjustments: { ...INITIAL_ADJUSTMENTS, exposure: -1 } });
  await resolve(Invokes.LoadImage, '/second.dng', { ...nativeResult, width: 6000 });
  expect(useEditorStore.getState().selectedImage).toMatchObject({ path: '/second.dng', isReady: true, width: 6000 });
  expect(useEditorStore.getState().adjustments.exposure).toBe(-1);
});

it('does not clear a replacement selection when the previous source fails', async () => {
  renderHook(() => useImageLoader({ current: null }));
  await act(async () => {});
  const first = pending.find((item) => item.command === Invokes.LoadImage && item.path === '/first.dng')!;
  first.settled = true;
  await act(async () => {
    useEditorStore.setState({ selectedImage: photo('/second.dng') });
    first.reject(new Error('obsolete decode failed'));
    await Promise.resolve();
  });
  expect(useEditorStore.getState().selectedImage).toMatchObject({ path: '/second.dng', isReady: false });
  expect(useLibraryStore.getState().isViewLoading).toBe(true);
  expect(toast.error).not.toHaveBeenCalled();
});

it('shows the priority camera JPEG before RAW completion without enabling edits or rendering it as RAW', async () => {
  renderHook(() => useImageLoader({ current: null }));
  await resolve(Invokes.LoadEmbeddedPreview, '/first.dng', new Uint8Array([255, 216, 255, 217]).buffer);
  expect(useEditorStore.getState().selectedImage?.thumbnailUrl).toBe('');
  await resolve(Invokes.LoadMetadata, '/first.dng', { adjustments: null });
  expect(useEditorStore.getState().selectedImage).toMatchObject({
    path: '/first.dng',
    isReady: false,
    thumbnailUrl: 'blob:priority-preview',
    thumbnailSource: 'embedded'
  });
  expect(useEditorStore.getState().finalPreview).toBeNull();
  expect(useEditorStore.getState().hasRenderedFirstFrame).toBe(false);
  await resolve(Invokes.LoadImage, '/first.dng', nativeResult);
  expect(useEditorStore.getState().selectedImage?.isReady).toBe(true);
  expect(URL.revokeObjectURL).not.toHaveBeenCalled();
});

it('does not request a camera preview when a rendered cached thumbnail is already available', () => {
  useEditorStore.setState({ selectedImage: { ...photo('/first.dng'), thumbnailUrl: 'asset://edited.jpg' } });
  renderHook(() => useImageLoader({ current: null }));
  expect(pending.some(({ command }) => command === Invokes.LoadEmbeddedPreview)).toBe(false);
});

it('ignores priority previews for obsolete selections and after the final source becomes ready', async () => {
  renderHook(() => useImageLoader({ current: null }));
  act(() => useEditorStore.setState({ selectedImage: photo('/second.dng') }));
  await resolve(Invokes.LoadMetadata, '/first.dng', { adjustments: null });
  // The obsolete selection does not even ask for its camera JPEG.
  expect(pending.some((r) => r.command === Invokes.LoadEmbeddedPreview && r.path === '/first.dng')).toBe(false);
  expect(URL.createObjectURL).not.toHaveBeenCalled();
  await resolve(Invokes.LoadMetadata, '/second.dng', { adjustments: null });
  await resolve(Invokes.LoadImage, '/second.dng', nativeResult);
  await resolve(Invokes.LoadEmbeddedPreview, '/second.dng', new Uint8Array([1]).buffer);
  expect(URL.createObjectURL).not.toHaveBeenCalled();
  expect(useEditorStore.getState().selectedImage).toMatchObject({
    path: '/second.dng',
    isReady: true,
    thumbnailUrl: ''
  });
});

it('keeps camera JPEGs out of a recipe with saved rotation or crop', async () => {
  renderHook(() => useImageLoader({ current: null }));
  await resolve(Invokes.LoadMetadata, '/first.dng', { adjustments: { orientationSteps: 1 } });
  await resolve(Invokes.LoadEmbeddedPreview, '/first.dng', new Uint8Array([1]).buffer);
  expect(URL.createObjectURL).not.toHaveBeenCalled();
  act(() => useEditorStore.setState({ selectedImage: photo('/second.dng') }));
  await resolve(Invokes.LoadMetadata, '/second.dng', { adjustments: { crop: { x: 1, y: 2, width: 3, height: 4 } } });
  await resolve(Invokes.LoadEmbeddedPreview, '/second.dng', new Uint8Array([1]).buffer);
  expect(URL.createObjectURL).not.toHaveBeenCalled();
});

it('releases a transient camera preview when an unfinished selection is replaced', async () => {
  renderHook(() => useImageLoader({ current: null }));
  await resolve(Invokes.LoadMetadata, '/first.dng', { adjustments: null });
  await resolve(Invokes.LoadEmbeddedPreview, '/first.dng', new Uint8Array([1]).buffer);
  act(() => useEditorStore.setState({ selectedImage: photo('/second.dng') }));
  expect(URL.revokeObjectURL).toHaveBeenCalledWith('blob:priority-preview');
});

it('reopens with the existing undo orders while the queue is matching its kept History', async () => {
  const path = '/first.dng';
  const before = editorRecipe({ ...INITIAL_ADJUSTMENTS, exposure: 0.5 }, true, 4000, 3000, null);
  const parameters = { maskDataBase64: null, detectPending: true };
  const withSky = (parameters: object) =>
    editorRecipe(
      { ...before, masks: [{ id: 'layer', subMasks: [{ id: 'sky', type: 'ai-sky', parameters }] }] },
      true,
      4000,
      3000,
      null
    );
  const waiting = withSky(parameters);
  const computed = { maskDataBase64: 'data:sky' };
  const saved = withSky(computed);
  const history = [before, waiting];
  const orders = [nextUndoOrder(), nextUndoOrder()];
  setHistoryOrders(history, orders);
  useEditorStore.setState({
    selectedImage: { ...photo(path), isReady: true },
    adjustments: waiting,
    history,
    historyIndex: 1
  });
  useEditorStore.setState({ selectedImage: null });
  const adopting = adoptQueuedSelections(path);
  await act(async () => {});
  useEditorStore.setState({ selectedImage: photo(path) });
  renderHook(() => useImageLoader({ current: null }));
  await resolve(Invokes.LoadMetadata, path, { adjustments: saved, exif: {} });
  await resolve(Invokes.LoadImage, path, nativeResult);

  await resolve(Invokes.AdoptSelections, path, [{ sky: computed }]);
  const reopened = useEditorStore.getState();
  expect(reopened.selectedImage?.isReady).toBe(true);
  expect(historyOrders(reopened.history)).toEqual(orders);
  expect(reopened.history[reopened.historyIndex]).toBe(reopened.adjustments);
  expect(await adopting).toBe(true);
});

it('waits for the reopened photograph’s pending save before reading its recipe', async () => {
  const edited = { ...INITIAL_ADJUSTMENTS, exposure: 2.75 };
  debouncedSave('/first.dng', edited);
  debouncedSave.flush();
  renderHook(() => useImageLoader({ current: null }));
  await act(async () => {});
  expect(pending.map(({ command }) => command)).toEqual([
    Invokes.SaveMetadataAndUpdateThumbnail,
    Invokes.LoadEmbeddedPreview,
    Invokes.LoadImage
  ]);
  // Decoding can finish while persistence is still busy; that old native
  // metadata must not replace the freshly edited recipe cached in the UI.
  await resolve(Invokes.LoadImage, '/first.dng', {
    ...nativeResult,
    metadata: { adjustments: INITIAL_ADJUSTMENTS }
  });
  expect(useEditorStore.getState().selectedImage?.isReady).toBe(false);
  await resolve(Invokes.SaveMetadataAndUpdateThumbnail, '/first.dng', undefined);
  expect(pending.at(-1)?.command).toBe(Invokes.LoadMetadata);
  await resolve(Invokes.LoadMetadata, '/first.dng', { adjustments: edited });
  expect(useEditorStore.getState().selectedImage?.isReady).toBe(true);
  expect(useEditorStore.getState().adjustments.exposure).toBe(2.75);
});

it('serializes repeated saves of one photograph before releasing metadata readers', async () => {
  debouncedSave('/serial.dng', { ...INITIAL_ADJUSTMENTS, exposure: 1 });
  debouncedSave.flush();
  debouncedSave('/serial.dng', { ...INITIAL_ADJUSTMENTS, exposure: 2 });
  debouncedSave.flush();
  expect(pending).toHaveLength(1);
  const ready = vi.fn();
  void waitForPendingMetadataSave('/serial.dng')!.then(ready);
  await resolve(Invokes.SaveMetadataAndUpdateThumbnail, '/serial.dng', undefined);
  expect(ready).not.toHaveBeenCalled();
  expect(pending).toHaveLength(2);
  expect(vi.mocked(invoke).mock.calls.at(-1)?.[1]).toMatchObject({ adjustments: { exposure: 2 } });
  await resolve(Invokes.SaveMetadataAndUpdateThumbnail, '/serial.dng', undefined);
  expect(ready).toHaveBeenCalledExactlyOnceWith(undefined);
});

/** WQA7: loading the open photograph again (selected again, back after an
 * undone Move) wrote its full recipe beside a linked original. */
it('saves nothing when the open photograph loads again unchanged', async () => {
  const renderRefs = {
    previewJobIdRef: { current: 0 },
    latestRenderedJobIdRef: { current: 0 },
    currentResRef: { current: 0 }
  };
  const previous = { current: null };
  renderHook(() => {
    useImageLoader({ current: null });
    useImageProcessing({ current: null }, previous, renderRefs);
  });
  await act(async () => {});
  const rated = { rating: 3, adjustments: null };
  await resolve(Invokes.LoadMetadata, '/first.dng', rated);
  await resolve(Invokes.LoadImage, '/first.dng', nativeResult);
  const first = useEditorStore.getState().adjustments;
  act(() => useEditorStore.setState({ selectedImage: null }));
  act(() => useEditorStore.setState({ selectedImage: photo('/first.dng') }));
  await act(async () => {});
  await resolve(Invokes.LoadMetadata, '/first.dng', rated);
  await resolve(Invokes.LoadImage, '/first.dng', nativeResult);
  expect(useEditorStore.getState().selectedImage?.isReady).toBe(true);
  expect(useEditorStore.getState().adjustments).not.toBe(first);
  expect(useEditorStore.getState().adjustments).toEqual(first);
  // A save held back would go now.
  await act(async () => debouncedSave.flush());
  await act(async () => {});
  expect(
    vi.mocked(invoke).mock.calls.filter(([command]) => command === Invokes.SaveMetadataAndUpdateThumbnail)
  ).toHaveLength(0);
});

it('preserves the unsaved recipe on reopen after a failed write and reports the error', async () => {
  const errorLog = vi.spyOn(console, 'error').mockImplementation(() => {});
  resetActivityStore();
  resetActivityBridges();
  const stopActivity = startActivityBridges();
  useEditorStore.setState({ selectedImage: photo('/failed.dng') });
  const edited = { ...INITIAL_ADJUSTMENTS, exposure: 3.5 };
  debouncedSave('/failed.dng', edited);
  debouncedSave.flush();
  renderHook(() => useImageLoader({ current: null }));
  const save = pending.find((request) => request.command === Invokes.SaveMetadataAndUpdateThumbnail)!;
  save.settled = true;
  await act(async () => save.reject(new Error('disk full')));
  const failure = useActivityStore.getState().entries.find((entry) => entry.id === 'write:not-saved');
  expect(failure).toMatchObject({ status: 'failure', paths: ['/failed.dng'] });
  expect(failure?.detail).toContain('disk full');
  stopActivity();
  await resolve(Invokes.LoadMetadata, '/failed.dng', { adjustments: INITIAL_ADJUSTMENTS });
  await resolve(Invokes.LoadImage, '/failed.dng', nativeResult);
  expect(useEditorStore.getState().selectedImage?.isReady).toBe(true);
  expect(useEditorStore.getState().adjustments.exposure).toBe(3.5);
  expect(await waitForPendingMetadataSave('/failed.dng')).toEqual({
    ...edited,
    frameSize: { width: 4000, height: 3000 }
  });
  // Once persistence succeeds, subsequent metadata reads need no local overlay.
  debouncedSave('/failed.dng', edited);
  debouncedSave.flush();
  await resolve(Invokes.SaveMetadataAndUpdateThumbnail, '/failed.dng', undefined);
  expect(waitForPendingMetadataSave('/failed.dng')).toBeUndefined();
  errorLog.mockRestore();
});

it('starts a never-edited RAW with the camera lens data, but not an edited one', async () => {
  vi.mocked(invoke).mockImplementation((command, args) => {
    if (command === Invokes.ApplyAdjustments) return Promise.resolve(new ArrayBuffer(0));
    if (command === Invokes.GetCameraLensProfile)
      return Promise.resolve({
        maker: 'Fujifilm',
        vignetting: true,
        params: { k1: -0.02, k2: 0.05, k3: -0.04, model: 1 }
      });
    if (command === Invokes.LoadImageProxy) return Promise.resolve(null);
    return new Promise((resolve, reject) =>
      pending.push({ command, path: (args as { path: string }).path, resolve, reject })
    );
  });
  useEditorStore.setState({ selectedImage: photo('/new.raf') });
  renderHook(() => useImageLoader({ current: null }));
  await resolve(Invokes.LoadMetadata, '/new.raf', { adjustments: null });
  await resolve(Invokes.LoadEmbeddedPreview, '/new.raf', null);
  await resolve(Invokes.LoadImage, '/new.raf', nativeResult);
  expect(useEditorStore.getState().selectedImage?.isReady).toBe(true);
  expect(useEditorStore.getState().adjustments.lensCorrectionMode).toBe('camera');
  expect(useEditorStore.getState().adjustments.fringeVersion).toBe(FRINGE_VERSION);
  expect(useEditorStore.getState().adjustments.cameraCaVersion).toBe(CAMERA_CA_VERSION);
  expect(useEditorStore.getState().adjustments.cameraGeometryVersion).toBe(CAMERA_GEOMETRY_VERSION);
  expect(useEditorStore.getState().adjustments.autoCaVersion).toBe(AUTO_CA_VERSION);
  expect(useEditorStore.getState().history).toHaveLength(1);

  useEditorStore.setState({ selectedImage: photo('/edited.raf') });
  await act(async () => {});
  await resolve(Invokes.LoadMetadata, '/edited.raf', {
    adjustments: { ...INITIAL_ADJUSTMENTS, lensCorrectionMode: 'manual' }
  });
  await resolve(Invokes.LoadEmbeddedPreview, '/edited.raf', null);
  await resolve(Invokes.LoadImage, '/edited.raf', nativeResult);
  expect(useEditorStore.getState().adjustments.lensCorrectionMode).toBe('manual');
  expect(useEditorStore.getState().adjustments.lensDistortionParams).toBeNull();
  // A saved edit keeps rendering without the edge-fringe pass.
  expect(useEditorStore.getState().adjustments.fringeVersion).toBeUndefined();
  expect(useEditorStore.getState().adjustments.autoCaVersion).toBeUndefined();
});

it('renders from the preview proxy before the decode, then from the decode without resetting the recipe', async () => {
  proxyResult = { width: 4000, height: 3000, isRaw: true };
  const renderRefs = {
    previewJobIdRef: { current: 0 },
    latestRenderedJobIdRef: { current: 0 },
    currentResRef: { current: 0 }
  };
  renderHook(() => {
    useImageLoader({ current: null });
    useImageProcessing({ current: null }, { current: null }, renderRefs);
  });
  const previews = () => vi.mocked(invoke).mock.calls.filter(([command]) => command === Invokes.ApplyAdjustments);
  await resolve(Invokes.LoadMetadata, '/first.dng', { adjustments: { ...INITIAL_ADJUSTMENTS, exposure: 1.5 } });
  let state = useEditorStore.getState();
  // Rendered from the proxy and editable at once; the decode is still running.
  expect(state.selectedImage).toMatchObject({ isPreviewReady: true, isReady: true, width: 4000 });
  expect(state.selectedImage?.isDecoded).toBeFalsy();
  expect(state.adjustments).toMatchObject({ exposure: 1.5 });
  expect(previews()).toHaveLength(1);
  // The decode waits for the proxy's first frame, so it does not take the
  // cores that frame needs.
  expect(pending.some(({ command }) => command === Invokes.LoadImage)).toBe(false);
  act(() =>
    useEditorStore.setState({ history: [state.adjustments, { ...state.adjustments, exposure: 2 }], historyIndex: 1 })
  );
  await act(async () => useEditorStore.setState({ hasRenderedFirstFrame: true }));
  await resolve(Invokes.LoadImage, '/first.dng', nativeResult);
  state = useEditorStore.getState();
  expect(state.selectedImage).toMatchObject({ isPreviewReady: true, isReady: true, isDecoded: true });
  // Edits made on the proxy stay.
  expect(state.historyIndex).toBe(1);
  // The decode renders again from the full source.
  expect(previews()).toHaveLength(2);
});

it('shows the draft its decode hands over before the decode is in', async () => {
  const renderRefs = {
    previewJobIdRef: { current: 0 },
    latestRenderedJobIdRef: { current: 0 },
    currentResRef: { current: 0 }
  };
  renderHook(() => {
    useImageLoader({ current: null });
    useImageProcessing({ current: null }, { current: null }, renderRefs);
  });
  const previews = () => vi.mocked(invoke).mock.calls.filter(([command]) => command === Invokes.ApplyAdjustments);
  await resolve(Invokes.LoadMetadata, '/first.dng', { adjustments: { ...INITIAL_ADJUSTMENTS, exposure: 1.5 } });
  // No stored proxy: the decode starts at once, listening for its draft.
  expect(pending.some(({ command }) => command === Invokes.LoadImage)).toBe(true);
  expect(useEditorStore.getState().selectedImage?.isReady).toBe(false);
  // Another photograph's draft is not this one's.
  await act(async () => emitDraft!({ path: '/other.dng', width: 10, height: 10, isRaw: true }));
  expect(useEditorStore.getState().selectedImage?.isReady).toBe(false);
  await act(async () => emitDraft!({ path: '/first.dng', width: 4002, height: 3000, isRaw: true }));
  let state = useEditorStore.getState();
  expect(state.selectedImage).toMatchObject({ isPreviewReady: true, isReady: true, width: 4002 });
  expect(state.selectedImage?.isDecoded).toBeFalsy();
  expect(previews()).toHaveLength(1);
  await resolve(Invokes.LoadImage, '/first.dng', nativeResult);
  state = useEditorStore.getState();
  // The decode's size wins; the default aspect follows it.
  expect(state.selectedImage).toMatchObject({ isDecoded: true, width: 4000 });
  expect(state.adjustments.aspectRatio).toBe(4000 / 3000);
  expect(state.adjustments.exposure).toBe(1.5);
});

it('plans exports from the saved recipe when an untouched photo is opened and left in Develop', async () => {
  const first = '/first.dng';
  const second = '/second.dng';
  const planned: Record<string, unknown>[] = [];
  const saved: Record<string, unknown> = {};
  const backend = vi.mocked(invoke).getMockImplementation()!;
  vi.mocked(invoke).mockImplementation((command, args) => {
    if (command === Invokes.SaveMetadataAndUpdateThumbnail) {
      const { path, adjustments } = args as { path: string; adjustments: unknown };
      saved[path] = adjustments;
      return Promise.resolve(undefined);
    }
    if (command === Invokes.PlanExportCollisions) {
      planned.push((args as { requests: Record<string, unknown>[] }).requests[0]);
      return Promise.resolve({ total: 0 });
    }
    if (command === Invokes.ExportImagesBatch) return Promise.resolve(undefined);
    return backend(command, args);
  });
  const draft = { ...DEFAULT_RECIPE_DRAFT, where: 'original' as const, existingFiles: 'ask' as const };
  const exportBoth = () => runExportJobs([{ recipe: null, draft, ...exportSettingsFromDraft(draft) }], [first, second]);

  await expect(exportBoth()).resolves.toBe(1);
  const previous = { current: null };
  const renderRefs = {
    previewJobIdRef: { current: 0 },
    latestRenderedJobIdRef: { current: 0 },
    currentResRef: { current: 0 }
  };
  renderHook(() => {
    useImageLoader({ current: null });
    useImageProcessing({ current: null }, previous, renderRefs);
  });
  await resolve(Invokes.LoadMetadata, first, { adjustments: null });
  await act(async () => emitDraft!({ path: first, width: 4002, height: 3000, isRaw: true }));
  await resolve(Invokes.LoadImage, first, nativeResult);
  const { adjustments, history, historyIndex } = useEditorStore.getState();
  expect(adjustments.aspectRatio).toBe(4000 / 3000);
  expect(history[historyIndex]).toBe(adjustments);
  await expect(exportBoth()).resolves.toBe(1);

  act(() => useEditorStore.setState({ selectedImage: photo(second) }));
  await resolve(Invokes.LoadMetadata, second, { adjustments: null });
  await resolve(Invokes.LoadImage, second, nativeResult);
  act(() => updateAdjustments({ exposure: 1 }));
  await expect(exportBoth()).resolves.toBe(1);

  expect(saved).toMatchObject({ [second]: { exposure: 1 } });
  expect(saved).not.toHaveProperty([first]);
  expect(planned).toMatchObject([
    { currentEditPath: null, currentEditAdjustments: null },
    { currentEditPath: first, currentEditAdjustments: null },
    { currentEditPath: second, currentEditAdjustments: null }
  ]);
});

it('keeps an offline photograph open on its preview and says which one', async () => {
  proxyResult = { width: 4000, height: 3000, isRaw: true };
  renderHook(() => useImageLoader({ current: null }));
  await resolve(Invokes.LoadMetadata, '/first.dng', { adjustments: { ...INITIAL_ADJUSTMENTS, exposure: 0.5 } });
  await act(async () => useEditorStore.setState({ hasRenderedFirstFrame: true }));
  await resolve(Invokes.LoadImage, '/first.dng', { ...nativeResult, offline: 'standard' });
  const state = useEditorStore.getState();
  expect(state.selectedImage).toMatchObject({
    path: '/first.dng',
    isReady: true,
    isDecoded: true,
    offline: 'standard'
  });
  expect(state.adjustments.exposure).toBe(0.5);
  expect(toast.error).not.toHaveBeenCalled();
});

it('starts the decode after a stored proxy’s first frame, or after a short wait without one', async () => {
  vi.useFakeTimers();
  try {
    let settled = false;
    void waitForFirstFrame(PROXY_FRAME_FIRST_MS).then(() => {
      settled = true;
    });
    await vi.advanceTimersByTimeAsync(PROXY_FRAME_FIRST_MS - 1);
    expect(settled).toBe(false);
    await vi.advanceTimersByTimeAsync(1);
    expect(settled).toBe(true);
    settled = false;
    void waitForFirstFrame(PROXY_FRAME_FIRST_MS).then(() => {
      settled = true;
    });
    useEditorStore.setState({ hasRenderedFirstFrame: true });
    await vi.advanceTimersByTimeAsync(0);
    expect(settled).toBe(true);
  } finally {
    vi.useRealTimers();
  }
});

it('never starts the decode of a photograph left while its proxy frame was awaited', async () => {
  vi.useFakeTimers();
  try {
    proxyResult = { width: 4000, height: 3000, isRaw: true };
    renderHook(() => useImageLoader({ current: null }));
    await act(async () => {});
    act(() => useEditorStore.setState({ selectedImage: photo('/second.dng') }));
    await act(async () => vi.advanceTimersByTimeAsync(PROXY_FRAME_FIRST_MS));
    const decodes = pending.filter(({ command }) => command === Invokes.LoadImage).map(({ path }) => path);
    expect(decodes).toEqual(['/second.dng']);
  } finally {
    vi.useRealTimers();
  }
});
