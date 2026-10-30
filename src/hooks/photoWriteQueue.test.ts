// Every write of a photograph goes through its one queue (utils/photoWrites):
// the races of separate save paths that did not know each other.
import { act, cleanup, renderHook } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { debouncedSave, debouncedSetHistory, flushRecipeSaves, useEditorActions, waitForPendingMetadataSave } from './useEditorActions';
import { useEditorStore } from '../store/useEditorStore';
import { useLibraryStore } from '../store/useLibraryStore';
import { useSettingsStore } from '../store/useSettingsStore';
import { INITIAL_ADJUSTMENTS, PasteMode } from '../utils/adjustments';
import { AppSettings, Invokes } from '../components/ui/AppProperties';
import { invokeInOrder } from '../utils/catalogWrites';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('react-toastify', () => ({ toast: { error: vi.fn() } }));

const SETTLED = 'rawtonic:photo-writes-settled';
type Pending = { command: string; args: unknown; resolve(value?: unknown): void; reject(error: unknown): void; settled?: boolean };
let pending: Pending[];
const commands = () => pending.map(({ command }) => command);
const answer = async (command: string, value?: unknown, fail = false) => {
  await act(async () => {});
  const request = pending.find((request) => !request.settled && request.command === command);
  if (!request) throw new Error(`${command} was not asked for; asked: ${commands().join(', ')}`);
  request.settled = true;
  await act(async () => (fail ? request.reject(value) : request.resolve(value)));
};

beforeEach(() => {
  pending = [];
  vi.mocked(invoke).mockImplementation(
    (command, args) => new Promise((resolve, reject) => pending.push({ command, args, resolve, reject })),
  );
  const adjustments = { ...INITIAL_ADJUSTMENTS, exposure: -2 };
  useEditorStore.setState({
    selectedImage: { path: '/open.dng', isReady: true, isRaw: true, width: 4000, height: 3000, exif: {}, thumbnailUrl: '' } as never,
    adjustments, history: [adjustments], historyIndex: 0,
    copiedAdjustments: { ...INITIAL_ADJUSTMENTS, exposure: 3 }, copiedFrom: '/source.dng',
  });
  useLibraryStore.setState({ libraryActivePath: '/open.dng', multiSelectedPaths: ['/open.dng'] });
  useSettingsStore.setState({ appSettings: {
    copyPasteSettings: { mode: PasteMode.Replace, includedAdjustments: ['exposure'] },
  } as AppSettings });
});
afterEach(async () => {
  cleanup();
  debouncedSave.cancel();
  debouncedSetHistory.cancel();
  // Nothing of one test may still wait in the queue of the next.
  for (const request of pending) if (!request.settled) request.resolve(undefined);
  await act(async () => {});
  vi.clearAllMocks();
});

it('saves an edit made during a Reset after the Reset, so the Reset cannot overwrite it', async () => {
  const { result } = renderHook(useEditorActions);
  act(() => result.current.handleResetAdjustments(['/closed.dng']));
  await act(async () => {});
  expect(commands()).toEqual([Invokes.ResetAdjustmentsForPaths]);
  debouncedSave('/closed.dng', { ...INITIAL_ADJUSTMENTS, exposure: 1 });
  debouncedSave.flush();
  await act(async () => {});
  // Submitted later, it waits: written first, the slower Reset would land last.
  expect(commands()).toEqual([Invokes.ResetAdjustmentsForPaths]);
  await answer(Invokes.ResetAdjustmentsForPaths);
  await vi.waitFor(() => expect(commands()).toContain(Invokes.SaveMetadataAndUpdateThumbnail));
});

it('applies batch writes of one photograph in the order they were given', async () => {
  const { result } = renderHook(useEditorActions);
  act(() => result.current.handleResetAdjustments(['/a.dng', '/b.dng']));
  await act(async () => { await result.current.handlePasteAdjustments(['/b.dng']); });
  // The paste onto B waits for B's Reset, not only for B's editor save.
  expect(commands()).toEqual([Invokes.ResetAdjustmentsForPaths]);
  await answer(Invokes.ResetAdjustmentsForPaths);
  await vi.waitFor(() => expect(commands()).toEqual([Invokes.ResetAdjustmentsForPaths, Invokes.ApplyAdjustmentsToPaths]));
});

it('orders an autosave, a batch and the next autosave of one photograph without waiting in a cycle', async () => {
  const { result } = renderHook(useEditorActions);
  debouncedSave('/a.dng', { ...INITIAL_ADJUSTMENTS, exposure: 1 });
  debouncedSave.flush();
  await act(async () => { await result.current.handlePasteAdjustments(['/a.dng', '/b.dng']); });
  await act(async () => { await result.current.handlePasteAdjustments(['/a.dng']); });
  debouncedSave('/a.dng', { ...INITIAL_ADJUSTMENTS, exposure: 2 });
  debouncedSave.flush();
  await answer(Invokes.SaveMetadataAndUpdateThumbnail);
  await answer(Invokes.ApplyAdjustmentsToPaths);
  await answer(Invokes.ApplyAdjustmentsToPaths);
  await answer(Invokes.SaveMetadataAndUpdateThumbnail);
  expect(commands()).toEqual([
    Invokes.SaveMetadataAndUpdateThumbnail,
    Invokes.ApplyAdjustmentsToPaths,
    Invokes.ApplyAdjustmentsToPaths,
    Invokes.SaveMetadataAndUpdateThumbnail,
  ]);
  expect(pending.at(-1)!.args).toMatchObject({ adjustments: { exposure: 2 } });
  expect(await flushRecipeSaves()).toEqual([]);
});

it('opens a photograph only after its batch write, so it never shows the recipe before it', async () => {
  const { result } = renderHook(useEditorActions);
  act(() => result.current.handleResetAdjustments(['/closed.dng']));
  const read = waitForPendingMetadataSave('/closed.dng');
  expect(read).toBeDefined();
  let done = false;
  void read!.then(() => { done = true; });
  await act(async () => {});
  expect(done).toBe(false);
  await answer(Invokes.ResetAdjustmentsForPaths);
  expect(done).toBe(true);
});

it('waits for batch writes before quitting', async () => {
  const { result } = renderHook(useEditorActions);
  act(() => result.current.handleAutoLensCorrection(['/closed.dng']));
  let quit = false;
  const flushing = flushRecipeSaves().then(() => { quit = true; });
  await act(async () => {});
  expect(quit).toBe(false);
  await answer('apply_auto_lens_correction_to_paths');
  await flushing;
  expect(quit).toBe(true);
});

it('writes a rating after the recipe save given before it, so the older XMP cannot land last', async () => {
  debouncedSave('/a.dng', { ...INITIAL_ADJUSTMENTS, exposure: 1 });
  debouncedSave.flush();
  const rated = invokeInOrder(Invokes.SetRatingForPaths, { paths: ['/a.dng'], rating: 4 });
  await act(async () => {});
  expect(commands()).toEqual([Invokes.SaveMetadataAndUpdateThumbnail]);
  await answer(Invokes.SaveMetadataAndUpdateThumbnail);
  await answer(Invokes.SetRatingForPaths, []);
  await rated;
});

it('announces a batch paste once when it settled, naming the photographs it could not write', async () => {
  const settled = vi.fn();
  window.addEventListener(SETTLED, settled);
  try {
    vi.spyOn(console, 'error').mockImplementation(() => {});
    const { result } = renderHook(useEditorActions);
    await act(async () => { await result.current.handlePasteAdjustments(['/a.dng', '/b.dng']); });
    await answer(Invokes.ApplyAdjustmentsToPaths, { message: 'Could not save 1 of 2 photos', failed: ['/b.dng'] }, true);
    await act(async () => {});
    expect(settled).toHaveBeenCalledTimes(1);
    expect((settled.mock.calls[0][0] as CustomEvent).detail).toMatchObject({
      kind: 'paste', paths: ['/a.dng', '/b.dng'], failed: ['/b.dng'],
    });
  } finally {
    window.removeEventListener(SETTLED, settled);
  }
});

it('announces each editor save once, a failed one with its photograph', async () => {
  const settled = vi.fn();
  window.addEventListener(SETTLED, settled);
  try {
    vi.spyOn(console, 'error').mockImplementation(() => {});
    debouncedSave('/a.dng', { ...INITIAL_ADJUSTMENTS, exposure: 1 });
    debouncedSave.flush();
    await answer(Invokes.SaveMetadataAndUpdateThumbnail, new Error('disk full'), true);
    await act(async () => {});
    expect(settled).toHaveBeenCalledTimes(1);
    expect((settled.mock.calls[0][0] as CustomEvent).detail).toMatchObject({ kind: 'recipe', paths: ['/a.dng'], failed: ['/a.dng'] });
  } finally {
    window.removeEventListener(SETTLED, settled);
  }
});

it('keeps a batch in the queue until every retry of a failed save has settled, also when one fails at once', async () => {
  vi.spyOn(console, 'error').mockImplementation(() => {});
  // Two editor saves fail, so the paste retries both first.
  for (const path of ['/a.dng', '/b.dng']) {
    debouncedSave(path, { ...INITIAL_ADJUSTMENTS, exposure: 1 });
    debouncedSave.flush();
    await answer(Invokes.SaveMetadataAndUpdateThumbnail, new Error('disk full'), true);
  }
  const { result } = renderHook(useEditorActions);
  await act(async () => { await result.current.handlePasteAdjustments(['/a.dng', '/b.dng']); });
  await act(async () => {});
  expect(commands().filter((command) => command === Invokes.SaveMetadataAndUpdateThumbnail)).toHaveLength(4);
  // A's retry fails at once; B's is still writing.
  await answer(Invokes.SaveMetadataAndUpdateThumbnail, new Error('disk full'), true);
  debouncedSave('/b.dng', { ...INITIAL_ADJUSTMENTS, exposure: 2 });
  debouncedSave.flush();
  let quit = false;
  void flushRecipeSaves().then(() => { quit = true; });
  await act(async () => {});
  // B's newer save and quitting wait for B's retry.
  expect(commands().filter((command) => command === Invokes.SaveMetadataAndUpdateThumbnail)).toHaveLength(4);
  expect(quit).toBe(false);
  await answer(Invokes.SaveMetadataAndUpdateThumbnail);
  await vi.waitFor(() => expect(pending.at(-1)!.args).toMatchObject({ path: '/b.dng', adjustments: { exposure: 2 } }));
  expect(commands()).not.toContain(Invokes.ApplyAdjustmentsToPaths);
});
