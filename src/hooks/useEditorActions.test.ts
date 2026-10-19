import { act, cleanup, renderHook } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import {
  debouncedSave,
  debouncedSetHistory,
  endBeforePeek,
  flushRecipeSaves,
  registerScheduledRecipe,
  startBeforePeek,
  updateAdjustments,
  useEditorActions,
  waitForPendingMetadataSave,
} from './useEditorActions';
import { useEditorStore } from '../store/useEditorStore';
import { useLibraryStore } from '../store/useLibraryStore';
import { useSettingsStore } from '../store/useSettingsStore';
import { useMultiViewStore } from '../store/useMultiViewStore';
import { INITIAL_ADJUSTMENTS, PasteMode } from '../utils/adjustments';
import { AppSettings, Invokes } from '../components/ui/AppProperties';
import { COMMIT_INTERVAL_MS, registerSliderDraftRenderer } from '../utils/sliderDraft';
import { toast } from 'react-toastify';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('react-toastify', () => ({ toast: { error: vi.fn() } }));

type Pending = { command: string; resolve(value: unknown): void; settled?: boolean };
let pending: Pending[];
const photo = (path: string, isReady = true) => ({
  path, isReady, isRaw: true, width: 4000, height: 3000, exif: {}, thumbnailUrl: '',
});
const select = (path: string, isReady = true) => {
  const adjustments = { ...INITIAL_ADJUSTMENTS, exposure: -2 };
  useEditorStore.setState({ selectedImage: photo(path, isReady), adjustments, history: [adjustments], historyIndex: 0 });
  useLibraryStore.setState({ libraryActivePath: path, multiSelectedPaths: [path], libraryActiveAdjustments: adjustments });
};
const finish = async (command: string, value: unknown) => {
  await act(async () => {});
  const request = pending.find((request) => !request.settled && request.command === command)!;
  request.settled = true;
  await act(async () => request.resolve(value));
};

beforeEach(() => {
  pending = [];
  vi.mocked(invoke).mockImplementation((command) => new Promise((resolve) => pending.push({ command, resolve })));
  select('/first.dng');
  useEditorStore.setState({ copiedAdjustments: { ...INITIAL_ADJUSTMENTS, exposure: 3 } });
  useSettingsStore.setState({ appSettings: {
    copyPasteSettings: { mode: PasteMode.Replace, includedAdjustments: ['exposure'] },
  } as AppSettings });
});
afterEach(() => {
  cleanup();
  debouncedSave.cancel();
  debouncedSetHistory.cancel();
  vi.clearAllMocks();
});

it.each([
  ['auto', Invokes.CalculateAutoAdjustments, { ...INITIAL_ADJUSTMENTS, exposure: 4 }],
  ['lut', 'load_and_parse_lut', { size: 32 }],
  ['reset', Invokes.ResetAdjustmentsForPaths, undefined],
] as const)('ignores a completed %s edit after switching photographs', async (kind, command, payload) => {
  const { result } = renderHook(useEditorActions);
  act(() => {
    if (kind === 'auto') void result.current.handleAutoAdjustments();
    if (kind === 'lut') void result.current.handleLutSelect('/test.cube');
    if (kind === 'reset') result.current.handleResetAdjustments(['/first.dng']);
  });
  act(() => select('/second.dng'));
  const recipe = useEditorStore.getState().adjustments;
  await finish(command, payload);
  expect(useEditorStore.getState().adjustments).toBe(recipe);
  expect(useEditorStore.getState().history).toEqual([recipe]);
  expect(useLibraryStore.getState().libraryActiveAdjustments).toBe(recipe);
});

it('does not apply auto results to the same path while its source is loading', async () => {
  const { result } = renderHook(useEditorActions);
  act(() => { void result.current.handleAutoAdjustments(); });
  act(() => select('/first.dng', false));
  const recipe = useEditorStore.getState().adjustments;
  await finish(Invokes.CalculateAutoAdjustments, { ...INITIAL_ADJUSTMENTS, exposure: 4 });
  expect(useEditorStore.getState().adjustments).toBe(recipe);
});

it.each(['lens', 'paste'] as const)('ignores delayed %s metadata when the user switches during its reload', async (kind) => {
  const { result } = renderHook(useEditorActions);
  act(() => {
    if (kind === 'lens') result.current.handleAutoLensCorrection(['/first.dng']);
    else result.current.handlePasteAdjustments(['/first.dng']);
  });
  await finish(kind === 'lens' ? 'apply_auto_lens_correction_to_paths' : Invokes.ApplyAdjustmentsToPaths, undefined);
  expect(pending.at(-1)?.command).toBe(Invokes.LoadMetadata);
  act(() => select('/second.dng'));
  const recipe = useEditorStore.getState().adjustments;
  await finish(Invokes.LoadMetadata, { adjustments: { ...INITIAL_ADJUSTMENTS, exposure: 4, lensMaker: 'old camera' } });
  expect(useEditorStore.getState().adjustments).toBe(recipe);
  expect(useEditorStore.getState().history).toEqual([recipe]);
  expect(useLibraryStore.getState().libraryActiveAdjustments).toBe(recipe);
});

it('still applies an auto result when its photograph remains current and ready', async () => {
  const { result } = renderHook(useEditorActions);
  act(() => { void result.current.handleAutoAdjustments(); });
  await finish(Invokes.CalculateAutoAdjustments, { ...INITIAL_ADJUSTMENTS, exposure: 4 });
  expect(useEditorStore.getState().adjustments.exposure).toBe(4);
});

it('asks for the auto method of the edit\'s base look', async () => {
  const { result } = renderHook(useEditorActions);
  act(() => { void result.current.handleAutoAdjustments(); });
  await finish(Invokes.CalculateAutoAdjustments, { exposure: 0.5 });
  expect(invoke).toHaveBeenCalledWith(Invokes.CalculateAutoAdjustments, { toneMapper: 'standard', baseCurve: 'standard' });
  useEditorStore.setState({ adjustments: { ...INITIAL_ADJUSTMENTS, toneMapper: 'basic', baseCurve: 'linear' } });
  act(() => { void result.current.handleAutoAdjustments(); });
  await finish(Invokes.CalculateAutoAdjustments, { exposure: 0.5 });
  expect(invoke).toHaveBeenLastCalledWith(Invokes.CalculateAutoAdjustments, { toneMapper: 'basic', baseCurve: 'linear' });
});

it('puts the auto black point into an untouched luma curve only', async () => {
  const { result } = renderHook(useEditorActions);
  act(() => { void result.current.handleAutoAdjustments(); });
  await finish(Invokes.CalculateAutoAdjustments, { exposure: 0.4, autoBlackPoint: 6.5 });
  const state = useEditorStore.getState().adjustments;
  expect(state.curves.luma).toEqual([{ x: 6.5, y: 0 }, { x: 255, y: 255 }]);
  expect(state.autoBlackPoint).toBeUndefined();
  const userLuma = [{ x: 0, y: 10 }, { x: 128, y: 140 }, { x: 255, y: 255 }];
  useEditorStore.setState({ adjustments: { ...INITIAL_ADJUSTMENTS, curves: { ...INITIAL_ADJUSTMENTS.curves, luma: userLuma } } });
  act(() => { void result.current.handleAutoAdjustments(); });
  await finish(Invokes.CalculateAutoAdjustments, { exposure: 0.4, autoBlackPoint: 6.5 });
  expect(useEditorStore.getState().adjustments.curves.luma).toEqual(userLuma);
});

it('clears a failed-save overlay after an explicit successful reset from Library', async () => {
  const errorLog = vi.spyOn(console, 'error').mockImplementation(() => {});
  vi.mocked(invoke).mockRejectedValueOnce(new Error('disk full'));
  const unsaved = { ...INITIAL_ADJUSTMENTS, exposure: 3.5 };
  debouncedSave('/failed-reset.dng', unsaved);
  await act(async () => { await debouncedSave.flush(); });
  expect(await waitForPendingMetadataSave('/failed-reset.dng')).toBe(unsaved);
  const { result } = renderHook(useEditorActions);
  act(() => result.current.handleResetAdjustments(['/failed-reset.dng']));
  await finish(Invokes.ResetAdjustmentsForPaths, undefined);
  expect(waitForPendingMetadataSave('/failed-reset.dng')).toBeUndefined();
  expect(useEditorStore.getState().selectedImage?.path).toBe('/first.dng');
  expect(useEditorStore.getState().adjustments.exposure).toBe(-2);
  errorLog.mockRestore();
});

it('persists unrelated unsaved adjustments before applying a partial Library paste', async () => {
  const errorLog = vi.spyOn(console, 'error').mockImplementation(() => {});
  vi.mocked(invoke).mockRejectedValueOnce(new Error('disk full'));
  const unsaved = { ...INITIAL_ADJUSTMENTS, temperature: 42 };
  debouncedSave('/failed-paste.dng', unsaved);
  await act(async () => { await debouncedSave.flush(); });
  const { result } = renderHook(useEditorActions);
  await act(async () => result.current.handlePasteAdjustments(['/failed-paste.dng']));
  expect(pending.map(({ command }) => command)).toEqual([Invokes.SaveMetadataAndUpdateThumbnail]);
  expect(vi.mocked(invoke).mock.calls.at(-1)?.[1]).toMatchObject({ adjustments: { temperature: 42 } });
  await finish(Invokes.SaveMetadataAndUpdateThumbnail, undefined);
  expect(pending.at(-1)?.command).toBe(Invokes.ApplyAdjustmentsToPaths);
  await finish(Invokes.ApplyAdjustmentsToPaths, undefined);
  expect(waitForPendingMetadataSave('/failed-paste.dng')).toBeUndefined();
  errorLog.mockRestore();
});

it.each([
  ['the open photograph', ['/first.dng'], -2],
  ['another photograph', ['/other.dng'], 0],
] as const)('reports a reset that %s did not save and resets only the saved ones', async (_, failed, exposure) => {
  const errorLog = vi.spyOn(console, 'error').mockImplementation(() => {});
  const message = `Could not save 1 of 2 photos: ${failed[0]}: disk full`;
  vi.mocked(invoke).mockRejectedValueOnce({ message, failed });
  const { result } = renderHook(useEditorActions);
  act(() => result.current.handleResetAdjustments(['/first.dng', '/other.dng']));
  await act(async () => {});
  expect(toast.error).toHaveBeenCalledWith(`Failed to reset adjustments: ${message}`);
  expect(useEditorStore.getState().adjustments.exposure).toBe(exposure);
  errorLog.mockRestore();
});

it('makes Reset one undoable step for the open photograph', async () => {
  const { result } = renderHook(useEditorActions);
  act(() => result.current.handleResetAdjustments(['/first.dng']));
  await finish(Invokes.ResetAdjustmentsForPaths, undefined);
  expect(useEditorStore.getState().adjustments.exposure).toBe(0);
  expect(useEditorStore.getState().history).toHaveLength(2);
  act(() => useEditorStore.getState().undo());
  expect(useEditorStore.getState().adjustments.exposure).toBe(-2);
});

describe('canvas edits (crop, straighten, drawn masks) in the history', () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it('never land in the next photograph\'s history', () => {
    act(() => updateAdjustments((prev) => ({ ...prev, rotation: 3 })));
    expect(useEditorStore.getState().historyPending).toBe(true);
    act(() => select('/second.dng'));
    vi.advanceTimersByTime(1000);
    expect(useEditorStore.getState().history.map((r) => r.rotation)).toEqual([0]);
    expect(useEditorStore.getState().historyPending).toBe(false);
  });

  it('are settled by undo, so undo cannot write them back later', () => {
    act(() => updateAdjustments({ rotation: 3 }));
    act(() => useEditorStore.getState().undo());
    expect(useEditorStore.getState().adjustments.rotation).toBe(0);
    vi.advanceTimersByTime(1000);
    expect(useEditorStore.getState().adjustments.rotation).toBe(0);
    expect(useEditorStore.getState().history.map((r) => r.rotation)).toEqual([0, 3]);
  });

  it('wait for a ready photograph and skip updates that change nothing', () => {
    act(() => select('/first.dng', false));
    const before = useEditorStore.getState().adjustments;
    act(() => updateAdjustments({ rotation: 3 }));
    expect(useEditorStore.getState().adjustments).toBe(before);
    act(() => select('/first.dng'));
    act(() => updateAdjustments((prev) => prev));
    expect(useEditorStore.getState().historyPending).toBe(false);
  });
});

it('keeps the edit made just before Reset as its own undo step', async () => {
  const { result } = renderHook(useEditorActions);
  act(() => result.current.setAdjustments({ exposure: 1 }));
  act(() => result.current.handleResetAdjustments(['/first.dng']));
  await finish(Invokes.ResetAdjustmentsForPaths, undefined);
  expect(useEditorStore.getState().history.map((r) => r.exposure)).toEqual([-2, 1, 0]);
  act(() => useEditorStore.getState().undo());
  expect(useEditorStore.getState().adjustments.exposure).toBe(1);
});

it('writes a debounced recipe save at once before quitting and waits for it', async () => {
  const edited = { ...INITIAL_ADJUSTMENTS, exposure: 1.67 };
  debouncedSave('/quit.dng', edited);
  let flushed = false;
  const flushing = flushRecipeSaves().then((failed) => {
    flushed = true;
    return failed;
  });
  await act(async () => {});
  expect(pending.map(({ command }) => command)).toEqual([Invokes.SaveMetadataAndUpdateThumbnail]);
  expect(vi.mocked(invoke).mock.calls[0][1]).toEqual({ path: '/quit.dng', adjustments: edited });
  expect(flushed).toBe(false);
  await finish(Invokes.SaveMetadataAndUpdateThumbnail, undefined);
  expect(await flushing).toEqual([]);
});

it('saves the recipe of a slider still being dragged when quitting, never a half-loaded one', async () => {
  const taken = useEditorStore.getState().adjustments;
  const unregister = registerScheduledRecipe(() => ({ path: '/first.dng', adjustments: taken }));
  try {
    // Unchanged since the last scheduled save: nothing to write.
    expect(await flushRecipeSaves()).toEqual([]);
    expect(invoke).not.toHaveBeenCalled();
    // A drag in progress changed it; its save waits for the drag to end.
    const dragged = { ...taken, exposure: 1.2 };
    useEditorStore.setState({ adjustments: dragged, isSliderDragging: true });
    const flushing = flushRecipeSaves();
    await finish(Invokes.SaveMetadataAndUpdateThumbnail, undefined);
    expect(await flushing).toEqual([]);
    expect(vi.mocked(invoke).mock.calls[0][1]).toEqual({ path: '/first.dng', adjustments: dragged });
    // The next photograph is still loading: its recipe is not written.
    vi.mocked(invoke).mockClear();
    select('/second.dng', false);
    useEditorStore.setState({ adjustments: { ...taken, exposure: 3 } });
    expect(await flushRecipeSaves()).toEqual([]);
    expect(invoke).not.toHaveBeenCalled();
  } finally {
    unregister();
    useEditorStore.setState({ isSliderDragging: false });
  }
});

it('retries a failed recipe save before quitting and reports it if it fails again', async () => {
  const errorLog = vi.spyOn(console, 'error').mockImplementation(() => {});
  vi.mocked(invoke).mockRejectedValueOnce(new Error('disk full')).mockRejectedValueOnce(new Error('disk full'));
  debouncedSave('/unsaved.dng', { ...INITIAL_ADJUSTMENTS, exposure: 2 });
  expect(await flushRecipeSaves()).toEqual(['/unsaved.dng']);
  expect(vi.mocked(invoke)).toHaveBeenCalledTimes(2);
  const retried = flushRecipeSaves();
  await finish(Invokes.SaveMetadataAndUpdateThumbnail, undefined);
  expect(await retried).toEqual([]);
  expect(waitForPendingMetadataSave('/unsaved.dng')).toBeUndefined();
  errorLog.mockRestore();
});

describe('before/after split', () => {
  beforeEach(() => {
    useEditorStore.setState({ splitView: false, splitPosition: 0.5, showOriginal: false, previewOverride: null });
    useSettingsStore.setState({ osPlatform: 'macos' });
  });

  it('turns the split on and off, ending a held peek', () => {
    const { result } = renderHook(useEditorActions);
    act(() => void startBeforePeek());
    expect(useEditorStore.getState().showOriginal).toBe(true);
    act(() => result.current.toggleSplitView());
    const state = useEditorStore.getState();
    expect(state.splitView).toBe(true);
    expect(state.showOriginal).toBe(false);
    expect(state.previewOverride).toBeNull();
    act(() => result.current.toggleSplitView());
    expect(useEditorStore.getState().splitView).toBe(false);
  });

  it('peeks at the whole photograph before over the split and returns to it', () => {
    const { result } = renderHook(useEditorActions);
    act(() => result.current.toggleSplitView());
    expect(startBeforePeek()).toBe(true);
    expect(useEditorStore.getState()).toMatchObject({ splitView: true, showOriginal: true });
    // Already showing: a second start changes nothing (no latched toggle).
    expect(startBeforePeek()).toBe(false);
    endBeforePeek();
    expect(useEditorStore.getState()).toMatchObject({ splitView: true, showOriginal: false, previewOverride: null });
  });

  it('leaves a preset hover preview alone when no peek is showing', () => {
    const hover = { ...INITIAL_ADJUSTMENTS, exposure: 1 };
    useEditorStore.setState({ previewOverride: hover });
    endBeforePeek();
    expect(useEditorStore.getState().previewOverride).toBe(hover);
  });

  it('does not peek before the photograph is ready', () => {
    select('/loading.dng', false);
    expect(startBeforePeek()).toBe(false);
    expect(useEditorStore.getState().showOriginal).toBe(false);
  });

  it('keeps the split and its divider from photograph to photograph', () => {
    const { result } = renderHook(useEditorActions);
    act(() => result.current.toggleSplitView());
    useEditorStore.setState({ splitPosition: 0.3 });
    select('/second.dng');
    expect(useEditorStore.getState().splitView).toBe(true);
    expect(useEditorStore.getState().splitPosition).toBe(0.3);
  });

  it('shows the peek framed like the edit', () => {
    useEditorStore.setState({
      adjustments: { ...INITIAL_ADJUSTMENTS, exposure: 2, rotation: 3 },
    });
    startBeforePeek();
    const override = useEditorStore.getState().previewOverride!;
    expect(override.exposure).toBe(INITIAL_ADJUSTMENTS.exposure);
    expect(override.rotation).toBe(3);
  });

  it('allows Linux browser split, waits for a ready photograph, and always turns off', () => {
    const { result } = renderHook(useEditorActions);
    useSettingsStore.setState({ osPlatform: 'linux' });
    act(() => result.current.toggleSplitView());
    expect(useEditorStore.getState().splitView).toBe(true);
    act(() => result.current.toggleSplitView());
    useSettingsStore.setState({ osPlatform: 'macos' });
    select('/loading.dng', false);
    act(() => result.current.toggleSplitView());
    expect(useEditorStore.getState().splitView).toBe(false);
    useEditorStore.setState({ splitView: true });
    act(() => result.current.toggleSplitView());
    expect(useEditorStore.getState().splitView).toBe(false);
  });

  it('is single-view only: asking for it in the multi view turns the multi view off', () => {
    useMultiViewStore.setState({ enabled: true, active: true });
    const { result } = renderHook(useEditorActions);
    act(() => result.current.toggleSplitView());
    expect(useEditorStore.getState().splitView).toBe(true);
    expect(useMultiViewStore.getState().enabled).toBe(false);
    useMultiViewStore.setState({ enabled: true, active: false });
  });
});

it('never zooms out below fit from the keys, the zoom menu or the 100 % button', () => {
  // A 6000×4000 photograph fitted to 1500×1000: fit is 25 % on a 1× screen.
  useEditorStore.setState({
    originalSize: { width: 6000, height: 4000 },
    baseRenderSize: { width: 1500, height: 1000, offsetX: 0, offsetY: 0, containerWidth: 1500, containerHeight: 1000 },
    zoom: 2,
  });
  const { result } = renderHook(useEditorActions);
  act(() => result.current.handleZoomChange(0.1));
  expect(useEditorStore.getState().zoom).toBe(1);
  act(() => result.current.handleZoomChange(1));
  expect(useEditorStore.getState().zoom).toBeCloseTo(4);
  act(() => result.current.handleZoomChange(0, true));
  expect(useEditorStore.getState().zoom).toBe(1);
});

it('fits a large photograph in a small view (a multi view cell) without zooming it in', () => {
  // 8192×5464 in a 268×179 cell: fit is about 3.3 % per CSS px, under the
  // old 10 % (5 % per CSS px at 2×) floor that zoomed the primary in to 1.5.
  vi.stubGlobal('devicePixelRatio', 2);
  useEditorStore.setState({
    originalSize: { width: 8192, height: 5464 },
    baseRenderSize: { width: 268.4, height: 179, offsetX: 0.3, offsetY: 0, containerWidth: 269, containerHeight: 179 },
    zoom: 1.53,
  });
  const { result } = renderHook(useEditorActions);
  act(() => result.current.handleZoomChange(0, true));
  expect(useEditorStore.getState().zoom).toBeCloseTo(1);
  vi.unstubAllGlobals();
});

describe('slider drag drafts', () => {
  afterEach(() => {
    vi.useRealTimers();
    useEditorStore.setState({ isSliderDragging: false });
  });

  it('renders every move but commits on the interval and when the drag ends', () => {
    vi.useFakeTimers();
    const rendered: number[] = [];
    const unregister = registerSliderDraftRenderer((_, adjustments) => {
      rendered.push(adjustments.exposure);
      return true;
    });
    useEditorStore.setState({ isSliderDragging: true });
    updateAdjustments({ exposure: 0.5 });
    updateAdjustments((prev) => ({ ...prev, exposure: prev.exposure + 0.25 }));
    expect(rendered).toEqual([0.5, 0.75]);
    // Nothing committed yet; the second move built on the first.
    expect(useEditorStore.getState().adjustments.exposure).toBe(-2);
    vi.advanceTimersByTime(COMMIT_INTERVAL_MS);
    expect(useEditorStore.getState().adjustments.exposure).toBe(0.75);
    updateAdjustments({ exposure: 1 });
    expect(useEditorStore.getState().adjustments.exposure).toBe(0.75);
    useEditorStore.setState({ isSliderDragging: false });
    expect(useEditorStore.getState().adjustments.exposure).toBe(1);
    unregister();
  });

  it('commits a 1 s drag at 60 Hz at most four times a second, renders every move and ends on the last', () => {
    vi.useFakeTimers();
    const rendered: number[] = [];
    const unregister = registerSliderDraftRenderer((_, adjustments) => {
      rendered.push(adjustments.exposure);
      return true;
    });
    // Distinct committed recipes (a commit nested in the release's own
    // store update reaches a listener twice).
    const committed = new Set<unknown>();
    useEditorStore.setState({ isSliderDragging: true });
    const before = useEditorStore.getState().adjustments;
    const unsubscribe = useEditorStore.subscribe(({ adjustments }) => {
      if (adjustments !== before) committed.add(adjustments);
    });
    const moves = Array.from({ length: 60 }, (_, i) => -2 + (i + 1) / 20);
    for (const exposure of moves) {
      updateAdjustments({ exposure });
      vi.advanceTimersByTime(1000 / 60);
    }
    expect(rendered).toEqual(moves);
    // Linked displays (wheel puck, region curve) follow at 4 Hz, not 10.
    expect(COMMIT_INTERVAL_MS).toBe(250);
    // The interval restarts at the first move after a commit: 3 in the second.
    expect(committed.size).toBe(3);
    useEditorStore.setState({ isSliderDragging: false });
    expect(committed.size).toBe(4);
    expect(useEditorStore.getState().adjustments.exposure).toBe(moves[59]);
    // One undo step for the whole drag, as before.
    act(() => debouncedSetHistory.flush());
    expect(useEditorStore.getState().history.map((r) => r.exposure)).toEqual([-2, moves[59]]);
    unsubscribe();
    unregister();
  });

  it('commits each move when no renderer takes the draft', () => {
    const unregister = registerSliderDraftRenderer(() => false);
    useEditorStore.setState({ isSliderDragging: true });
    updateAdjustments({ exposure: 0.5 });
    expect(useEditorStore.getState().adjustments.exposure).toBe(0.5);
    unregister();
  });

  it('commits a pending draft before an edit outside the drag', () => {
    vi.useFakeTimers();
    const unregister = registerSliderDraftRenderer(() => true);
    useEditorStore.setState({ isSliderDragging: true });
    updateAdjustments({ exposure: 0.5 });
    useEditorStore.setState({ showOriginal: true });
    updateAdjustments({ contrast: 10 });
    const { adjustments } = useEditorStore.getState();
    expect([adjustments.exposure, adjustments.contrast]).toEqual([0.5, 10]);
    unregister();
  });
});

describe('Auto straighten', () => {
  const answer = { rotation: -2.4, crop: { unit: 'px', x: 120, y: 80, width: 3760, height: 2840 }, support: 1.2 };

  it('applies the rotation and crop it was asked for', async () => {
    const { result } = renderHook(useEditorActions);
    let outcome: string | undefined;
    act(() => { void result.current.handleAutoStraighten().then((o) => { outcome = o; }); });
    await finish(Invokes.CalculateAutoStraighten, answer);
    expect(outcome).toBe('done');
    expect(useEditorStore.getState().adjustments).toMatchObject({ rotation: -2.4, crop: answer.crop });
  });

  it('drops the answer when the crop changed meanwhile or the photograph is another', async () => {
    const { result } = renderHook(useEditorActions);
    const outcomes: string[] = [];
    act(() => { void result.current.handleAutoStraighten().then((o) => outcomes.push(o)); });
    act(() => useEditorStore.setState({
      adjustments: { ...useEditorStore.getState().adjustments, crop: { unit: 'px', x: 0, y: 0, width: 100, height: 100 } },
    }));
    await finish(Invokes.CalculateAutoStraighten, answer);
    act(() => { void result.current.handleAutoStraighten().then((o) => outcomes.push(o)); });
    act(() => select('/second.dng'));
    await finish(Invokes.CalculateAutoStraighten, answer);
    expect(outcomes).toEqual(['stale', 'stale']);
    expect(useEditorStore.getState().adjustments.rotation).not.toBe(-2.4);
  });

  it('asks for the open photograph and drops an answer about another', async () => {
    const { result } = renderHook(useEditorActions);
    let outcome: string | undefined;
    act(() => { void result.current.handleAutoStraighten().then((o) => { outcome = o; }); });
    await act(async () => {});
    expect(vi.mocked(invoke).mock.calls.at(-1)?.[1]).toMatchObject({ path: '/first.dng' });
    const request = pending.find((r) => r.command === Invokes.CalculateAutoStraighten)!;
    request.settled = true;
    await act(async () => request.resolve(Promise.reject('auto straighten: another photograph is open')));
    await act(async () => {});
    expect(outcome).toBe('stale');
    expect(useEditorStore.getState().adjustments.rotation).not.toBe(-2.4);
  });

  it('reports a photograph without straight lines', async () => {
    const { result } = renderHook(useEditorActions);
    let outcome: string | undefined;
    act(() => { void result.current.handleAutoStraighten().then((o) => { outcome = o; }); });
    await finish(Invokes.CalculateAutoStraighten, null);
    expect(outcome).toBe('none');
  });
});

describe('Auto straighten for a selection', () => {
  const report = { straightened: ['/first.dng'], noLines: [], changed: [], failed: [] };

  it('saves an edit made just before it first, holds the open photo, then reloads it as one step', async () => {
    const taken = useEditorStore.getState().adjustments;
    const unregister = registerScheduledRecipe(() => ({ path: '/first.dng', adjustments: taken }));
    try {
      // An edit whose 300 ms debounced save has not fired yet.
      const edited = { ...taken, exposure: 0.8 };
      useEditorStore.setState({ adjustments: edited });
      const { result } = renderHook(useEditorActions);
      let done: unknown;
      act(() => { void result.current.handleAutoStraightenPaths(['/first.dng', '/other.dng']).then((r) => { done = r; }); });
      await act(async () => {});
      expect(pending.map(({ command }) => command)).toEqual([Invokes.SaveMetadataAndUpdateThumbnail]);
      expect(vi.mocked(invoke).mock.calls[0][1]).toEqual({ path: '/first.dng', adjustments: edited });
      expect(useEditorStore.getState().heldRecipePaths).toEqual(['/first.dng', '/other.dng']);
      await finish(Invokes.SaveMetadataAndUpdateThumbnail, undefined);
      // Held: edits and undo do nothing while the batch writes.
      updateAdjustments({ exposure: 2 });
      act(() => useEditorStore.getState().undo());
      act(() => useEditorStore.getState().goToHistoryIndex(0));
      expect(useEditorStore.getState().adjustments).toBe(edited);
      await finish(Invokes.ApplyAutoStraightenToPaths, report);
      const straightened = { ...edited, rotation: -1.5, crop: { unit: 'px', x: 4, y: 3, width: 3990, height: 2994 } };
      await finish(Invokes.LoadMetadata, { adjustments: straightened });
      // The Library's active photograph is this one too.
      await finish(Invokes.LoadMetadata, { adjustments: straightened });
      expect(done).toEqual(report);
      expect(useEditorStore.getState().adjustments).toMatchObject({ rotation: -1.5, exposure: 0.8 });
      expect(useEditorStore.getState().heldRecipePaths).toEqual([]);
      // One undoable step back to the edit before it.
      act(() => useEditorStore.getState().undo());
      expect(useEditorStore.getState().adjustments.rotation ?? 0).toBe(0);
    } finally {
      unregister();
    }
  });

  it('holds a batch photograph opened while it runs and shows it the result', async () => {
    const { result } = renderHook(useEditorActions);
    let done: unknown;
    const both = { ...report, straightened: ['/first.dng', '/second.dng'] };
    act(() => { void result.current.handleAutoStraightenPaths(['/first.dng', '/second.dng']).then((r) => { done = r; }); });
    await act(async () => {});
    // The photographer opens the second photograph before its commit.
    act(() => select('/second.dng'));
    const before = useEditorStore.getState().adjustments;
    updateAdjustments({ exposure: 2 });
    expect(useEditorStore.getState().adjustments).toBe(before);
    await finish(Invokes.ApplyAutoStraightenToPaths, both);
    const straightened = { ...before, rotation: 3.1 };
    await finish(Invokes.LoadMetadata, { adjustments: straightened });
    await act(async () => {});
    expect(done).toEqual(both);
    expect(useEditorStore.getState().adjustments.rotation).toBe(3.1);
    expect(useEditorStore.getState().heldRecipePaths).toEqual([]);
  });

  it("keeps a never-edited RAW's default corrections when it shows the result", async () => {
    // The loader gave the never-edited photograph the camera's lens data; its
    // saved recipe (rotation and crop only after the batch) leaves it implicit.
    const shown = { ...useEditorStore.getState().adjustments, lensCorrectionMode: 'camera', lensDistortionParams: { k1: 0.02 } };
    useEditorStore.setState({ adjustments: shown as never });
    const { result } = renderHook(useEditorActions);
    act(() => { void result.current.handleAutoStraightenPaths(['/first.dng']); });
    await act(async () => {});
    await finish(Invokes.ApplyAutoStraightenToPaths, report);
    await finish(Invokes.LoadMetadata, {
      adjustments: { rotation: 2, crop: { unit: 'px', x: 5, y: 5, width: 3900, height: 2900 }, aiPatches: [{ id: 'p', framed: true }] },
    });
    await act(async () => {});
    const now = useEditorStore.getState().adjustments;
    expect(now.rotation).toBe(2);
    expect(now.crop).toMatchObject({ width: 3900 });
    expect(now.lensCorrectionMode).toBe('camera');
    expect(now.lensDistortionParams).toEqual({ k1: 0.02 });
    // Drawn masks and AI patches as the commit re-framed them.
    expect(now.aiPatches).toMatchObject([{ id: 'p', framed: true }]);
    // The Library's active photograph is this one too.
    await finish(Invokes.LoadMetadata, { adjustments: { rotation: 2 } });
  });

  it('applies a paste made during the batch after it, so neither is lost', async () => {
    const { result } = renderHook(useEditorActions);
    act(() => { void result.current.handleAutoStraightenPaths(['/first.dng']); });
    await act(async () => {});
    // Paste exposure 3 (beforeEach's clipboard) onto the held photo.
    act(() => result.current.handlePasteAdjustments(['/first.dng']));
    await act(async () => {});
    expect(pending.some((r) => r.command === Invokes.ApplyAdjustmentsToPaths)).toBe(false);
    await finish(Invokes.ApplyAutoStraightenToPaths, report);
    await finish(Invokes.LoadMetadata, { adjustments: { ...useEditorStore.getState().adjustments, rotation: -1.5 } });
    // The Library's active photograph is this one too.
    await finish(Invokes.LoadMetadata, { adjustments: { ...useEditorStore.getState().adjustments, rotation: -1.5 } });
    await act(async () => {});
    // Now the paste: written after the batch and shown in the editor.
    const paste = vi.mocked(invoke).mock.calls.find(([command]) => command === Invokes.ApplyAdjustmentsToPaths);
    expect(paste?.[1]).toMatchObject({ paths: ['/first.dng'], adjustments: { exposure: 3 } });
    expect(useEditorStore.getState().adjustments).toMatchObject({ rotation: -1.5, exposure: 3 });
    await finish(Invokes.ApplyAdjustmentsToPaths, undefined);
    await finish(Invokes.LoadMetadata, { adjustments: useEditorStore.getState().adjustments });
  });

  it('runs a second batch on the same photograph after the first, and a paste after both', async () => {
    const { result } = renderHook(useEditorActions);
    act(() => { void result.current.handleAutoStraightenPaths(['/first.dng', '/other.dng']); });
    await act(async () => {});
    act(() => { void result.current.handleAutoStraightenPaths(['/first.dng']); });
    act(() => result.current.handlePasteAdjustments(['/first.dng']));
    await act(async () => {});
    const commands = () => vi.mocked(invoke).mock.calls.map(([command]) => command);
    // Only the first batch runs; the second and the paste wait.
    expect(commands().filter((c) => c === Invokes.ApplyAutoStraightenToPaths)).toHaveLength(1);
    expect(commands()).not.toContain(Invokes.ApplyAdjustmentsToPaths);
    await finish(Invokes.ApplyAutoStraightenToPaths, report);
    await finish(Invokes.LoadMetadata, { adjustments: { ...useEditorStore.getState().adjustments, rotation: -1.5 } });
    await finish(Invokes.LoadMetadata, { adjustments: { ...useEditorStore.getState().adjustments, rotation: -1.5 } });
    await act(async () => {});
    // Now the second batch; the paste still waits for it.
    expect(commands().filter((c) => c === Invokes.ApplyAutoStraightenToPaths)).toHaveLength(2);
    expect(commands()).not.toContain(Invokes.ApplyAdjustmentsToPaths);
    expect(useEditorStore.getState().heldRecipePaths).toEqual(['/first.dng']);
    await finish(Invokes.ApplyAutoStraightenToPaths, report);
    await finish(Invokes.LoadMetadata, { adjustments: { ...useEditorStore.getState().adjustments, rotation: -1.6 } });
    await finish(Invokes.LoadMetadata, { adjustments: { ...useEditorStore.getState().adjustments, rotation: -1.6 } });
    await act(async () => {});
    expect(commands()).toContain(Invokes.ApplyAdjustmentsToPaths);
    expect(useEditorStore.getState().adjustments).toMatchObject({ rotation: -1.6, exposure: 3 });
    expect(useEditorStore.getState().heldRecipePaths).toEqual([]);
    await finish(Invokes.ApplyAdjustmentsToPaths, undefined);
    await finish(Invokes.LoadMetadata, { adjustments: useEditorStore.getState().adjustments });
  });

  it('runs Auto lens correction on a batch photograph after the batch, so its reload is not stale', async () => {
    const { result } = renderHook(useEditorActions);
    act(() => { void result.current.handleAutoStraightenPaths(['/first.dng']); });
    await act(async () => {});
    act(() => result.current.handleAutoLensCorrection(['/first.dng']));
    await act(async () => {});
    const commands = () => vi.mocked(invoke).mock.calls.map(([command]) => command);
    expect(commands()).not.toContain('apply_auto_lens_correction_to_paths');
    await finish(Invokes.ApplyAutoStraightenToPaths, report);
    await finish(Invokes.LoadMetadata, { adjustments: { ...useEditorStore.getState().adjustments, rotation: -3 } });
    await finish(Invokes.LoadMetadata, { adjustments: { ...useEditorStore.getState().adjustments, rotation: -3 } });
    await act(async () => {});
    expect(commands()).toContain('apply_auto_lens_correction_to_paths');
    await finish('apply_auto_lens_correction_to_paths', undefined);
    // Its reload reads the recipe after the batch: the rotation stays.
    await finish(Invokes.LoadMetadata, { adjustments: { ...useEditorStore.getState().adjustments, lensCorrectionMode: 'auto' } });
    await finish(Invokes.LoadMetadata, { adjustments: useEditorStore.getState().adjustments });
    expect(useEditorStore.getState().adjustments).toMatchObject({ rotation: -3, lensCorrectionMode: 'auto' });
  });

  it('runs a batch started right after a reset once the reset is done', async () => {
    const { result } = renderHook(useEditorActions);
    act(() => result.current.handleResetAdjustments(['/first.dng']));
    act(() => { void result.current.handleAutoStraightenPaths(['/first.dng']); });
    await act(async () => {});
    const commands = () => vi.mocked(invoke).mock.calls.map(([command]) => command);
    expect(commands()).toContain(Invokes.ResetAdjustmentsForPaths);
    expect(commands()).not.toContain(Invokes.ApplyAutoStraightenToPaths);
    await finish(Invokes.ResetAdjustmentsForPaths, undefined);
    await act(async () => {});
    expect(commands()).toContain(Invokes.ApplyAutoStraightenToPaths);
    await finish(Invokes.ApplyAutoStraightenToPaths, report);
    await finish(Invokes.LoadMetadata, { adjustments: { ...useEditorStore.getState().adjustments, rotation: -4 } });
    await finish(Invokes.LoadMetadata, { adjustments: { rotation: -4 } });
    // The reset's editor update came first; the batch's result stays.
    expect(useEditorStore.getState().adjustments.rotation).toBe(-4);
  });

  it('runs batch, reset and batch on one photograph in the order asked for', async () => {
    const { result } = renderHook(useEditorActions);
    const commands = () =>
      vi.mocked(invoke).mock.calls.map(([command]) => command)
        .filter((c) => c === Invokes.ApplyAutoStraightenToPaths || c === Invokes.ResetAdjustmentsForPaths);
    act(() => { void result.current.handleAutoStraightenPaths(['/first.dng']); });
    await act(async () => {});
    act(() => result.current.handleResetAdjustments(['/first.dng']));
    act(() => { void result.current.handleAutoStraightenPaths(['/first.dng']); });
    await act(async () => {});
    expect(commands()).toEqual([Invokes.ApplyAutoStraightenToPaths]);
    await finish(Invokes.ApplyAutoStraightenToPaths, report);
    await finish(Invokes.LoadMetadata, { adjustments: { ...useEditorStore.getState().adjustments, rotation: -1 } });
    await finish(Invokes.LoadMetadata, { adjustments: { rotation: -1 } });
    await act(async () => {});
    expect(commands()).toEqual([Invokes.ApplyAutoStraightenToPaths, Invokes.ResetAdjustmentsForPaths]);
    await finish(Invokes.ResetAdjustmentsForPaths, undefined);
    await act(async () => {});
    expect(commands()).toEqual([
      Invokes.ApplyAutoStraightenToPaths, Invokes.ResetAdjustmentsForPaths, Invokes.ApplyAutoStraightenToPaths,
    ]);
    await finish(Invokes.ApplyAutoStraightenToPaths, report);
    await finish(Invokes.LoadMetadata, { adjustments: { ...useEditorStore.getState().adjustments, rotation: -2 } });
    await finish(Invokes.LoadMetadata, { adjustments: { rotation: -2 } });
    // The later batch's result is what stays.
    expect(useEditorStore.getState().adjustments.rotation).toBe(-2);
  });

  it('shows a paste queued between two batches, though the later batch already holds the photo', async () => {
    const { result } = renderHook(useEditorActions);
    act(() => { void result.current.handleAutoStraightenPaths(['/first.dng']); });
    await act(async () => {});
    act(() => result.current.handlePasteAdjustments(['/first.dng']));
    act(() => { void result.current.handleAutoStraightenPaths(['/first.dng']); });
    await act(async () => {});
    await finish(Invokes.ApplyAutoStraightenToPaths, report);
    await finish(Invokes.LoadMetadata, { adjustments: { ...useEditorStore.getState().adjustments, rotation: -1 } });
    await finish(Invokes.LoadMetadata, { adjustments: { rotation: -1 } });
    await act(async () => {});
    // The paste runs now, under batch B's hold: the editor shows it.
    expect(useEditorStore.getState().adjustments.exposure).toBe(3);
    await finish(Invokes.ApplyAdjustmentsToPaths, undefined);
    await finish(Invokes.LoadMetadata, { adjustments: useEditorStore.getState().adjustments });
    await act(async () => {});
    await finish(Invokes.ApplyAutoStraightenToPaths, report);
    await finish(Invokes.LoadMetadata, { adjustments: { ...useEditorStore.getState().adjustments, rotation: -2 } });
    await finish(Invokes.LoadMetadata, { adjustments: { rotation: -2 } });
    expect(useEditorStore.getState().adjustments).toMatchObject({ rotation: -2, exposure: 3 });
    expect(useEditorStore.getState().heldRecipePaths).toEqual([]);
    // Undo takes back the second batch only: the paste keeps its own step.
    act(() => useEditorStore.getState().undo());
    expect(useEditorStore.getState().adjustments).toMatchObject({ rotation: -1, exposure: 3 });
  });

  it('holds the queue until a paste has also updated the editor', async () => {
    const { result } = renderHook(useEditorActions);
    act(() => result.current.handlePasteAdjustments(['/first.dng']));
    act(() => result.current.handleAutoLensCorrection(['/first.dng']));
    await finish(Invokes.ApplyAdjustmentsToPaths, undefined);
    await act(async () => {});
    const commands = () => vi.mocked(invoke).mock.calls.map(([command]) => command);
    // The paste's own reload is still out: Auto lens correction waits.
    expect(commands()).not.toContain('apply_auto_lens_correction_to_paths');
    await finish(Invokes.LoadMetadata, { adjustments: useEditorStore.getState().adjustments });
    await act(async () => {});
    expect(commands()).toContain('apply_auto_lens_correction_to_paths');
    await finish('apply_auto_lens_correction_to_paths', undefined);
    await finish(Invokes.LoadMetadata, { adjustments: useEditorStore.getState().adjustments });
    await finish(Invokes.LoadMetadata, { adjustments: useEditorStore.getState().adjustments });
  });

  it("saves the open photograph's edit though the hold moves the save baseline to it", async () => {
    const taken = useEditorStore.getState().adjustments;
    let baseline = taken;
    const unregister = registerScheduledRecipe(() => ({ path: '/first.dng', adjustments: baseline }));
    // As useImageProcessing does: a held recipe becomes the save baseline.
    const unsubscribe = useEditorStore.subscribe((state) => {
      if (state.heldRecipePaths.includes('/first.dng')) baseline = state.adjustments;
    });
    try {
      const edited = { ...taken, exposure: 0.8 };
      useEditorStore.setState({ adjustments: edited });
      const { result } = renderHook(useEditorActions);
      act(() => { void result.current.handleAutoStraightenPaths(['/first.dng']); });
      await act(async () => {});
      const save = vi.mocked(invoke).mock.calls.find(([command]) => command === Invokes.SaveMetadataAndUpdateThumbnail);
      expect(save?.[1]).toEqual({ path: '/first.dng', adjustments: edited });
      await finish(Invokes.SaveMetadataAndUpdateThumbnail, undefined);
      await finish(Invokes.ApplyAutoStraightenToPaths, report);
      await finish(Invokes.LoadMetadata, { adjustments: edited });
      await finish(Invokes.LoadMetadata, { adjustments: edited });
    } finally {
      unsubscribe();
      unregister();
    }
  });

  it('applies a queued paste to a photograph opened while it waited, once it has loaded', async () => {
    const { result } = renderHook(useEditorActions);
    act(() => { void result.current.handleAutoStraightenPaths(['/second.dng']); });
    await act(async () => {});
    act(() => result.current.handlePasteAdjustments(['/second.dng']));
    // The photographer opens it; it is still loading when the batch ends.
    act(() => select('/second.dng', false));
    await finish(Invokes.ApplyAutoStraightenToPaths, { ...report, straightened: [] });
    await act(async () => {});
    const commands = () => vi.mocked(invoke).mock.calls.map(([command]) => command);
    expect(commands()).not.toContain(Invokes.ApplyAdjustmentsToPaths);
    act(() => useEditorStore.setState({ selectedImage: { ...useEditorStore.getState().selectedImage!, isReady: true } }));
    await act(async () => {});
    expect(commands()).toContain(Invokes.ApplyAdjustmentsToPaths);
    expect(useEditorStore.getState().adjustments.exposure).toBe(3);
    await finish(Invokes.ApplyAdjustmentsToPaths, undefined);
    await finish(Invokes.LoadMetadata, { adjustments: useEditorStore.getState().adjustments });
  });

  it('flushes for quitting only after the queued recipe operations are done', async () => {
    const { result } = renderHook(useEditorActions);
    act(() => { void result.current.handleAutoStraightenPaths(['/first.dng']); });
    await act(async () => {});
    act(() => result.current.handlePasteAdjustments(['/first.dng']));
    let flushed = false;
    act(() => { void flushRecipeSaves().then(() => { flushed = true; }); });
    await finish(Invokes.ApplyAutoStraightenToPaths, report);
    await finish(Invokes.LoadMetadata, { adjustments: { ...useEditorStore.getState().adjustments, rotation: -1 } });
    await finish(Invokes.LoadMetadata, { adjustments: { rotation: -1 } });
    await act(async () => {});
    expect(flushed).toBe(false);
    await finish(Invokes.ApplyAdjustmentsToPaths, undefined);
    await finish(Invokes.LoadMetadata, { adjustments: useEditorStore.getState().adjustments });
    await act(async () => {});
    // The paste's editor update is saved before it resolves.
    const save = pending.find((r) => !r.settled && r.command === Invokes.SaveMetadataAndUpdateThumbnail);
    if (save) await finish(Invokes.SaveMetadataAndUpdateThumbnail, undefined);
    await act(async () => {});
    expect(flushed).toBe(true);
  });

  it('reloads the photograph opened while a queued Auto lens correction waited', async () => {
    const { result } = renderHook(useEditorActions);
    act(() => { void result.current.handleAutoStraightenPaths(['/second.dng']); });
    await act(async () => {});
    act(() => result.current.handleAutoLensCorrection(['/second.dng']));
    act(() => select('/second.dng'));
    await finish(Invokes.ApplyAutoStraightenToPaths, { ...report, straightened: ['/second.dng'] });
    await finish(Invokes.LoadMetadata, { adjustments: { ...useEditorStore.getState().adjustments, rotation: -1 } });
    await act(async () => {});
    await finish('apply_auto_lens_correction_to_paths', undefined);
    await finish(Invokes.LoadMetadata, {
      adjustments: { ...useEditorStore.getState().adjustments, rotation: -1, lensCorrectionMode: 'auto' },
    });
    await finish(Invokes.LoadMetadata, { adjustments: { rotation: -1, lensCorrectionMode: 'auto' } });
    expect(useEditorStore.getState().adjustments).toMatchObject({ rotation: -1, lensCorrectionMode: 'auto' });
  });

  it('updates a photograph being opened with a write that had to wait in its queue', async () => {
    const { result } = renderHook(useEditorActions);
    act(() => { void result.current.handleAutoStraightenPaths(['/second.dng']); });
    await act(async () => {});
    // Edit all selected writes while switching to the target (openingNow).
    act(() => { void result.current.applyAdjustmentsToPaths({ exposure: 1 }, ['/second.dng'], { openingNow: true }); });
    act(() => select('/second.dng', false));
    await finish(Invokes.ApplyAutoStraightenToPaths, { ...report, straightened: [] });
    await act(async () => {});
    const commands = () => vi.mocked(invoke).mock.calls.map(([command]) => command);
    // Its loader may have read the recipe before the write: it waits for the load.
    expect(commands()).not.toContain(Invokes.ApplyAdjustmentsToPaths);
    act(() => useEditorStore.setState({ selectedImage: { ...useEditorStore.getState().selectedImage!, isReady: true } }));
    await act(async () => {});
    expect(commands()).toContain(Invokes.ApplyAdjustmentsToPaths);
    expect(useEditorStore.getState().adjustments.exposure).toBe(1);
    await finish(Invokes.ApplyAdjustmentsToPaths, undefined);
    await finish(Invokes.LoadMetadata, { adjustments: useEditorStore.getState().adjustments });
  });

  it('keeps a queued paste waiting through a load longer than a minute', async () => {
    vi.useFakeTimers();
    try {
      const { result } = renderHook(useEditorActions);
      act(() => { void result.current.handleAutoStraightenPaths(['/second.dng']); });
      await act(async () => {});
      act(() => result.current.handlePasteAdjustments(['/second.dng']));
      act(() => select('/second.dng', false));
      await finish(Invokes.ApplyAutoStraightenToPaths, { ...report, straightened: [] });
      await act(async () => { await vi.advanceTimersByTimeAsync(120_000); });
      const commands = () => vi.mocked(invoke).mock.calls.map(([command]) => command);
      expect(commands()).not.toContain(Invokes.ApplyAdjustmentsToPaths);
      act(() => useEditorStore.setState({ selectedImage: { ...useEditorStore.getState().selectedImage!, isReady: true } }));
      await act(async () => {});
      expect(commands()).toContain(Invokes.ApplyAdjustmentsToPaths);
      expect(useEditorStore.getState().adjustments.exposure).toBe(3);
      await finish(Invokes.ApplyAdjustmentsToPaths, undefined);
      await finish(Invokes.LoadMetadata, { adjustments: useEditorStore.getState().adjustments });
    } finally {
      vi.useRealTimers();
    }
  });

  it('releases the hold when the batch fails', async () => {
    const { result } = renderHook(useEditorActions);
    let error: unknown;
    act(() => { void result.current.handleAutoStraightenPaths(['/first.dng']).catch((e) => { error = e; }); });
    await act(async () => {});
    const request = pending.find((r) => r.command === Invokes.ApplyAutoStraightenToPaths);
    expect(request).toBeDefined();
    await act(async () => {
      vi.mocked(invoke).mockImplementation(() => Promise.reject(new Error('unused')));
    });
    // Reject the batch.
    const failing = pending.find((r) => r.command === Invokes.ApplyAutoStraightenToPaths)!;
    failing.settled = true;
    await act(async () => (failing.resolve as (v: unknown) => void)(Promise.reject(new Error('disk full'))));
    await act(async () => {});
    expect(String(error)).toContain('disk full');
    expect(useEditorStore.getState().heldRecipePaths).toEqual([]);
  });
});
