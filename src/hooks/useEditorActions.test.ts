import { act, cleanup, renderHook } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import {
  debouncedSave,
  debouncedSetHistory,
  flushRecipeSaves,
  registerScheduledRecipe,
  useEditorActions,
  waitForPendingMetadataSave,
} from './useEditorActions';
import { useEditorStore } from '../store/useEditorStore';
import { useLibraryStore } from '../store/useLibraryStore';
import { useSettingsStore } from '../store/useSettingsStore';
import { INITIAL_ADJUSTMENTS, PasteMode } from '../utils/adjustments';
import { AppSettings, Invokes } from '../components/ui/AppProperties';
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

  it('turns the split on as an extra view and the quick original off', () => {
    const { result } = renderHook(useEditorActions);
    act(() => result.current.toggleShowOriginal());
    expect(useEditorStore.getState().showOriginal).toBe(true);
    act(() => result.current.toggleSplitView());
    const state = useEditorStore.getState();
    expect(state.splitView).toBe(true);
    expect(state.showOriginal).toBe(false);
    expect(state.previewOverride).toBeNull();
    act(() => result.current.toggleSplitView());
    expect(useEditorStore.getState().splitView).toBe(false);
  });

  it('keeps the split and its divider from photograph to photograph', () => {
    const { result } = renderHook(useEditorActions);
    act(() => result.current.toggleSplitView());
    useEditorStore.setState({ splitPosition: 0.3 });
    select('/second.dng');
    expect(useEditorStore.getState().splitView).toBe(true);
    expect(useEditorStore.getState().splitPosition).toBe(0.3);
  });

  it('shows the quick original framed like the edit', () => {
    useEditorStore.setState({
      adjustments: { ...INITIAL_ADJUSTMENTS, exposure: 2, rotation: 3 },
    });
    const { result } = renderHook(useEditorActions);
    act(() => result.current.toggleShowOriginal());
    const override = useEditorStore.getState().previewOverride!;
    expect(override.exposure).toBe(INITIAL_ADJUSTMENTS.exposure);
    expect(override.rotation).toBe(3);
  });

  it('does not turn on without the native display or a ready photograph, but always turns off', () => {
    const { result } = renderHook(useEditorActions);
    useSettingsStore.setState({ osPlatform: 'linux' });
    act(() => result.current.toggleSplitView());
    expect(useEditorStore.getState().splitView).toBe(false);
    useSettingsStore.setState({ osPlatform: 'macos' });
    select('/loading.dng', false);
    act(() => result.current.toggleSplitView());
    expect(useEditorStore.getState().splitView).toBe(false);
    useEditorStore.setState({ splitView: true });
    act(() => result.current.toggleSplitView());
    expect(useEditorStore.getState().splitView).toBe(false);
  });
});
