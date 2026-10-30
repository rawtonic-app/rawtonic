// A batch edit's undo and redo are batches of the per-photo write queue
// (utils/photoWrites) too: each photograph's restore joins its queue, so
// quitting, switching and exporting wait for it and failures surface like
// those of every other write (G26 O-01).
import { resetActivityBridges, startActivityBridges } from '../activity/bridges';
import { resetActivityStore, useActivityStore } from '../store/useActivityStore';
import { act, cleanup, renderHook } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { debouncedSave, debouncedSetHistory, flushRecipeSaves, useEditorActions, waitForPendingMetadataSave } from './useEditorActions';
import { useEditorStore } from '../store/useEditorStore';
import { useLibraryStore } from '../store/useLibraryStore';
import { INITIAL_ADJUSTMENTS } from '../utils/adjustments';
import { Invokes } from '../components/ui/AppProperties';
import { redo, undo, useCatalogUndoStore } from '../utils/appUndo';
import { PHOTO_WRITES_SETTLED, type PhotoWritesSettled } from '../utils/photoWrites';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('../activity/notify', () => ({ notify: { error: vi.fn(), info: vi.fn(), isActive: vi.fn(), update: vi.fn() } }));

type Pending = { command: string; args: unknown; resolve(value?: unknown): void; reject(error: unknown): void; settled?: boolean };
let pending: Pending[];
const commands = () => pending.map(({ command }) => command);
const answer = async (command: string, value?: unknown, fail = false) => {
  await act(async () => {});
  const request = pending.find((request) => !request.settled && request.command === command);
  if (!request) throw new Error(`${command} was not asked for; asked: ${commands().join(', ')}`);
  request.settled = true;
  await act(async () => (fail ? request.reject(value) : request.resolve(value)));
  return request;
};

const photos = ['/a.dng', '/b.dng', '/c.dng'];
const before = (path: string) => ({ ...INITIAL_ADJUSTMENTS, exposure: photos.indexOf(path) });
const after = (path: string) => ({ ...before(path), contrast: 20 });
/** The Sync's answer: every photograph changed. */
const synced = { changes: photos.map((path) => ({ path, before: before(path), after: after(path) })) };
let settled: PhotoWritesSettled[];
const onSettled = (event: Event) => settled.push((event as CustomEvent<PhotoWritesSettled>).detail);

/** Syncs the copied settings from the open photograph to the three others. */
async function sync() {
  const { result } = renderHook(useEditorActions);
  let done!: Promise<unknown>;
  act(() => {
    done = result.current.applyAdjustmentsToPaths({ contrast: 20 }, photos, { pasteFrom: '/open.dng', label: 'sync' })!;
  });
  await answer(Invokes.ApplyAdjustmentsToPaths, synced);
  await act(async () => {
    await done;
  });
}

beforeEach(() => {
  pending = [];
  settled = [];
  window.addEventListener(PHOTO_WRITES_SETTLED, onSettled);
  vi.mocked(invoke).mockImplementation(
    (command, args) => new Promise((resolve, reject) => pending.push({ command, args, resolve, reject })),
  );
  const adjustments = { ...INITIAL_ADJUSTMENTS };
  useEditorStore.setState({
    selectedImage: { path: '/open.dng', isReady: true, isRaw: true, width: 4000, height: 3000, exif: {}, thumbnailUrl: '' } as never,
    adjustments, history: [adjustments], historyIndex: 0,
  });
  useLibraryStore.setState({ libraryActivePath: '/open.dng', multiSelectedPaths: ['/open.dng'] });
  useCatalogUndoStore.setState({ past: [], future: [] });
});
afterEach(async () => {
  window.removeEventListener(PHOTO_WRITES_SETTLED, onSettled);
  cleanup();
  debouncedSave.cancel();
  debouncedSetHistory.cancel();
  for (const request of pending) if (!request.settled) request.resolve({ changes: [] });
  await act(async () => {});
  // Recipes a test left unsaved are written, so the next starts clean.
  vi.mocked(invoke).mockImplementation(() => Promise.resolve({ changes: [] }));
  await act(async () => {
    await flushRecipeSaves();
  });
  vi.clearAllMocks();
});

it('undoes a Sync over three photographs in one step through their write queues, and redoes it', async () => {
  await sync();
  expect(useCatalogUndoStore.getState().past).toHaveLength(1);
  settled = [];
  const undone = act(() => undo());
  const restore = await answer(Invokes.ReplaceRecipes, { changes: synced.changes.map(({ path, after: a, before: b }) => ({ path, before: a, after: b })) });
  await undone;
  expect(restore.args).toEqual({ changes: photos.map((path) => ({ path, adjustments: before(path) })) });
  // One write of the queue for all three, tagged with the step.
  expect(settled).toEqual([expect.objectContaining({ kind: 'restore', paths: photos, failed: [] })]);
  expect(settled[0].step).toEqual(expect.any(Number));
  expect(useCatalogUndoStore.getState().past).toHaveLength(0);
  expect(useCatalogUndoStore.getState().future).toHaveLength(1);

  const redone = act(() => redo());
  const again = await answer(Invokes.ReplaceRecipes, synced);
  await redone;
  expect(again.args).toEqual({ changes: photos.map((path) => ({ path, adjustments: after(path) })) });
  expect(useCatalogUndoStore.getState().past).toHaveLength(1);
});

it('writes the undo after a later write of one of its photographs that is still queued', async () => {
  await sync();
  // A save of B given after the Sync, not yet written.
  debouncedSave('/b.dng', { ...after('/b.dng'), exposure: 5 });
  debouncedSave.flush();
  await act(async () => {});
  expect(commands().at(-1)).toBe(Invokes.SaveMetadataAndUpdateThumbnail);
  const undone = act(() => undo());
  await act(async () => {});
  // B's restore waits in B's queue: it must not land before the save.
  expect(commands()).not.toContain(Invokes.ReplaceRecipes);
  await answer(Invokes.SaveMetadataAndUpdateThumbnail);
  await answer(Invokes.ReplaceRecipes, { changes: synced.changes });
  await undone;
  expect(commands().slice(-2)).toEqual([Invokes.SaveMetadataAndUpdateThumbnail, Invokes.ReplaceRecipes]);
});

it('reports a restore that failed for one photograph and keeps it unsaved for quitting', async () => {
  resetActivityStore();
  resetActivityBridges();
  const stopActivity = startActivityBridges();
  await sync();
  settled = [];
  const undone = act(() => undo());
  await answer(Invokes.ReplaceRecipes, {
    changes: synced.changes.filter(({ path }) => path !== '/b.dng'),
    failure: { message: 'disk full', failed: ['/b.dng'] },
  });
  await undone;
  expect(settled).toEqual([expect.objectContaining({ kind: 'restore', failed: ['/b.dng'] })]);
  const failure = useActivityStore.getState().entries.find((entry) => entry.id === 'write:not-saved');
  expect(failure).toMatchObject({ status: 'failure', paths: ['/b.dng'] });
  expect(failure?.detail).toContain('disk full');
  stopActivity();
  // The step stays to try again, and quitting names B when it still fails.
  expect(useCatalogUndoStore.getState().past).toHaveLength(1);
  let unsaved!: Promise<string[]>;
  await act(async () => {
    unsaved = flushRecipeSaves();
  });
  const retry = await answer(Invokes.SaveMetadataAndUpdateThumbnail, new Error('disk full'), true);
  expect(retry.args).toEqual({ path: '/b.dng', adjustments: before('/b.dng') });
  expect(await unsaved).toEqual(['/b.dng']);
});

it('makes a photograph opened meanwhile wait for a batch still waiting for an earlier step', async () => {
  const { result } = renderHook(useEditorActions);
  await sync();
  // The undo waits for nothing; a Reset of B given after it waits for the undo.
  const undone = act(() => undo());
  act(() => result.current.handleResetAdjustments(['/b.dng']));
  let read = false;
  void waitForPendingMetadataSave('/b.dng')!.then(() => { read = true; });
  await answer(Invokes.ReplaceRecipes, { changes: synced.changes });
  await undone;
  await act(async () => {});
  // Opening B reads its recipe only after the Reset wrote it.
  expect(read).toBe(false);
  await answer(Invokes.ResetAdjustmentsForPaths, { changes: [] });
  await vi.waitFor(() => expect(read).toBe(true));
});

it('keeps a failed restore of a never-edited recipe unsaved and tries it again when quitting', async () => {
  const { result } = renderHook(useEditorActions);
  let done!: Promise<unknown>;
  act(() => {
    done = result.current.applyAdjustmentsToPaths({ contrast: 20 }, ['/new.dng'], { pasteFrom: '/open.dng' })!;
  });
  await answer(Invokes.ApplyAdjustmentsToPaths, { changes: [{ path: '/new.dng', before: null, after: after('/new.dng') }] });
  await act(async () => { await done; });
  const undone = act(() => undo());
  await answer(Invokes.ReplaceRecipes, { changes: [], failure: { message: 'disk full', failed: ['/new.dng'] } });
  await undone;
  let unsaved!: Promise<string[]>;
  await act(async () => {
    unsaved = flushRecipeSaves();
  });
  const retry = await answer(Invokes.ReplaceRecipes, { changes: [], failure: { message: 'disk full', failed: ['/new.dng'] } });
  expect(retry.args).toEqual({ changes: [{ path: '/new.dng', adjustments: null }] });
  expect(await unsaved).toEqual(['/new.dng']);
});
