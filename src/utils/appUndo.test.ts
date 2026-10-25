import { beforeEach, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { debouncedSetHistory, useEditorStore } from '../store/useEditorStore';
import { useLibraryStore } from '../store/useLibraryStore';
import { useUIStore } from '../store/useUIStore';
import { INITIAL_ADJUSTMENTS } from './adjustments';
import { catalogAction, undo, redo, canRedo, getUndoLabel, useCatalogUndoStore } from './appUndo';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn(async () => []) }));
vi.mock('react-toastify', () => ({ toast: { error: vi.fn() } }));
const changes = [
  { path: '/a.nef', before: { kind: 'rating' as const, value: 0 }, after: { kind: 'rating' as const, value: 4 } },
];
beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(invoke).mockImplementation(async (_command, args) =>
    ((args as { values: { path: string; value: unknown }[] })?.values ?? []).map(({ path, value }) => ({
      path,
      before: value,
      after: value,
    })),
  );
  debouncedSetHistory.cancel();
  useEditorStore.setState(useEditorStore.getInitialState());
  useEditorStore.setState({ selectedImage: { path: '/a.nef', isReady: true } as never });
  useLibraryStore.setState({ sessionDirectory: '/job', imageRatings: {}, imageFlags: {}, imageList: [] });
  useUIStore.setState({ activeView: 'editor' });
  useCatalogUndoStore.setState({ past: [], future: [], busy: false });
});
it('orders rate → edit → undo → undo, then redoes in the same order', async () => {
  await catalogAction('Set Rating', async () => changes);
  const next = { ...INITIAL_ADJUSTMENTS, exposure: 1 };
  useEditorStore.getState().setEditor({ adjustments: next });
  debouncedSetHistory(next);
  expect(getUndoLabel()).toBe('Undo Edit');
  await undo();
  expect(useEditorStore.getState().adjustments.exposure).toBe(0);
  expect(getUndoLabel()).toBe('Undo Set Rating');
  await undo();
  expect(invoke).toHaveBeenLastCalledWith('restore_culling', {
    values: [{ path: '/a.nef', value: changes[0].before }],
    directory: '/job',
  });
  await redo();
  await redo();
  expect(useEditorStore.getState().adjustments.exposure).toBe(1);
});
it('undo after auto-advance changes the rated photo without moving the current view', async () => {
  await catalogAction('Set Rating', async () => changes);
  useEditorStore.getState().setEditor({ selectedImage: { path: '/b.nef', isReady: true } as never });
  useEditorStore.getState().resetHistory(INITIAL_ADJUSTMENTS);
  useLibraryStore.setState({ multiSelectedPaths: ['/b.nef'], libraryActivePath: '/b.nef' });
  await undo();
  expect(useEditorStore.getState().selectedImage?.path).toBe('/b.nef');
  expect(useLibraryStore.getState().multiSelectedPaths).toEqual(['/b.nef']);
  expect(invoke).toHaveBeenLastCalledWith('restore_culling', expect.anything());
});
it('keeps pending writes in input order and waits for them before undo', async () => {
  let finish!: (value: typeof changes) => void;
  const pending = catalogAction(
    'Set Rating',
    () =>
      new Promise<typeof changes>((resolve) => {
        finish = resolve;
      }),
  );
  const undone = undo();
  expect(invoke).not.toHaveBeenCalled();
  await Promise.resolve();
  finish(changes);
  await pending;
  await undone;
  expect(invoke).toHaveBeenCalledWith('restore_culling', expect.anything());
});
it('a new edit discards metadata redo', async () => {
  await catalogAction('Set Rating', async () => changes);
  await undo();
  useEditorStore.getState().pushHistory({ ...INITIAL_ADJUSTMENTS, exposure: 2 });
  expect(useCatalogUndoStore.getState().future).toEqual([]);
});

it('restores mixed ratings and no colour label without replacing keywords', async () => {
  const batch = [
    changes[0],
    { path: '/b.nef', before: { kind: 'rating' as const, value: 2 }, after: { kind: 'rating' as const, value: 4 } },
  ];
  useLibraryStore.setState({ imageRatings: { '/a.nef': 4, '/b.nef': 4 } });
  await catalogAction('Set Rating', async () => batch);
  await undo();
  expect(useLibraryStore.getState().imageRatings).toEqual({ '/a.nef': 0, '/b.nef': 2 });
  useLibraryStore.setState({ imageList: [{ path: '/a.nef', tags: ['user:beach', 'color:red'] }] as never });
  await catalogAction('Set Color Label', async () => [
    { path: '/a.nef', before: { kind: 'color', value: null }, after: { kind: 'color', value: 'red' } },
  ]);
  await undo();
  expect(useLibraryStore.getState().imageList[0].tags).toEqual(['user:beach']);
});
it('a partial action records only saved photos, and a partial undo remains retryable', async () => {
  await catalogAction('Set Rating', async () => {
    throw { culling: changes, failed: ['/b.nef'] };
  }).catch(() => {});
  vi.mocked(invoke).mockRejectedValueOnce({ message: 'offline', culling: [] });
  await undo();
  expect(getUndoLabel()).toBe('Undo Set Rating');
  await undo();
  expect(useCatalogUndoStore.getState().past).toHaveLength(0);
  expect(useCatalogUndoStore.getState().future[0].changes).toEqual(changes);
});
it('does not resurrect old history or indicators when a library closes during undo', async () => {
  await catalogAction('Set Rating', async () => changes);
  let finish!: (value: unknown) => void;
  vi.mocked(invoke).mockImplementationOnce(
    () =>
      new Promise((resolve) => {
        finish = resolve;
      }),
  );
  const undone = undo();
  // app queue, catalog write queue, native call
  await vi.waitFor(() => expect(finish).toBeTypeOf('function'));
  useLibraryStore.setState({ sessionDirectory: '/another', imageRatings: {} });
  finish([{ ...changes[0], after: changes[0].before }]);
  await undone;
  expect(useCatalogUndoStore.getState().future).toEqual([]);
  expect(useLibraryStore.getState().imageRatings).toEqual({});
});

it('an edit made while catalog undo waits starts a new branch without reviving metadata redo', async () => {
  await catalogAction('Set Rating', async () => changes);
  let finish!: (value: unknown) => void;
  vi.mocked(invoke).mockImplementationOnce(
    () =>
      new Promise((resolve) => {
        finish = resolve;
      }),
  );
  const undone = undo();
  await vi.waitFor(() => expect(finish).toBeTypeOf('function'));
  useEditorStore.getState().pushHistory({ ...INITIAL_ADJUSTMENTS, exposure: 2 });
  finish([{ ...changes[0], after: changes[0].before }]);
  await undone;
  expect(useCatalogUndoStore.getState().future).toEqual([]);
});
it('sends stable asset identity back to the native restore helper', async () => {
  await catalogAction('Set Rating', async () => [{ ...changes[0], assetId: 'asset-a' }]);
  await undo();
  expect(invoke).toHaveBeenCalledWith('restore_culling', {
    directory: '/job',
    values: [{ path: '/a.nef', assetId: 'asset-a', value: changes[0].before }],
  });
});

it('a pending edit cannot redo the catalog branch it replaces', async () => {
  await catalogAction('Set Rating', async () => changes);
  await undo();
  vi.mocked(invoke).mockClear();
  const next = { ...INITIAL_ADJUSTMENTS, exposure: 3 };
  useEditorStore.getState().setEditor({ adjustments: next });
  debouncedSetHistory(next);
  expect(canRedo()).toBe(false);
  await redo();
  expect(invoke).not.toHaveBeenCalled();
  debouncedSetHistory.flush();
  expect(useCatalogUndoStore.getState().future).toEqual([]);
});

it('orders a return to an evicted history entry ahead of catalog undo', async () => {
  for (let exposure = 1; exposure < 50; exposure++) {
    useEditorStore.getState().pushHistory({ ...INITIAL_ADJUSTMENTS, exposure });
  }
  await catalogAction('Set Rating', async () => changes);
  const pending = { ...INITIAL_ADJUSTMENTS, exposure: 50 };
  useEditorStore.getState().setEditor({ adjustments: pending });
  debouncedSetHistory(pending);
  // Committing the pending edit evicts the clicked baseline. Returning to
  // it becomes a new edit, with the pending edit kept immediately before it.
  useEditorStore.getState().goToHistoryIndex(0);
  expect(getUndoLabel()).toBe('Undo Edit');
  await undo();
  expect(useEditorStore.getState().adjustments.exposure).toBe(50);
  await undo();
  expect(useEditorStore.getState().adjustments.exposure).toBe(49);
  expect(getUndoLabel()).toBe('Undo Set Rating');
});
