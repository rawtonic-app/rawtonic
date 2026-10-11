import { beforeEach, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { followPathMoves, followRenamedPaths, movedPath, reloadSessionFlags, remapPathKeys } from './pathKeyedState';
import { useEditorStore } from '../store/useEditorStore';
import { useLibraryStore } from '../store/useLibraryStore';
import { useProcessStore } from '../store/useProcessStore';
import { useSettingsStore } from '../store/useSettingsStore';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
const mockInvoke = vi.mocked(invoke);

beforeEach(() => {
  mockInvoke.mockReset();
  useLibraryStore.setState({
    sessionDirectory: '/job',
    imageFlags: {},
    imageRatings: {},
    selectionAnchorPath: null,
  });
  useProcessStore.setState({ thumbnails: {} });
});

it('moves keys of renamed photographs and their virtual copies, and nothing else', () => {
  const moves = new Map([['/job/Capture/A.RAF', '/job/Capture/Pier.RAF']]);
  expect(
    remapPathKeys(
      { '/job/Capture/A.RAF': 1, '/job/Capture/A.RAF?vc=2': 3, '/job/Capture/B.RAF': -1 },
      moves,
    ),
  ).toEqual({ '/job/Capture/Pier.RAF': 1, '/job/Capture/Pier.RAF?vc=2': 3, '/job/Capture/B.RAF': -1 });
  // A stale entry at the new place gives way to the renamed photograph's.
  expect(remapPathKeys({ '/job/Capture/Pier.RAF': 0, '/job/Capture/A.RAF': 1 }, moves)).toEqual({
    '/job/Capture/Pier.RAF': 1,
  });
  const untouched = { '/job/Capture/B.RAF': 1 };
  expect(remapPathKeys(untouched, moves)).toBe(untouched);
});

it('keeps picks, ratings, thumbnails and the selection anchor with renamed photographs', () => {
  useLibraryStore.setState({
    imageFlags: { '/job/Capture/chart0.png': 1 },
    imageRatings: { '/job/Capture/chart0.png': 4, '/job/Capture/chart0.png?vc=1': 2 },
    selectionAnchorPath: '/job/Capture/chart0.png?vc=1',
  });
  useProcessStore.setState({ thumbnails: { '/job/Capture/chart0.png': 'blob:thumb' } });
  followRenamedPaths(
    ['/job/Capture/chart0.png', '/job/Capture/chart0.png?vc=1'],
    ['/job/Capture/renamed.png', '/job/Capture/renamed.png?vc=1'],
  );
  const library = useLibraryStore.getState();
  expect(library.imageFlags).toEqual({ '/job/Capture/renamed.png': 1 });
  expect(library.imageRatings).toEqual({ '/job/Capture/renamed.png': 4, '/job/Capture/renamed.png?vc=1': 2 });
  expect(library.selectionAnchorPath).toBe('/job/Capture/renamed.png?vc=1');
  expect(useProcessStore.getState().thumbnails).toEqual({ '/job/Capture/renamed.png': 'blob:thumb' });
});

it('moves everything in a moved folder with it, and nothing beside it', () => {
  const moves = new Map([
    ['/job/Day 1', '/job/Day 2'],
    ['C:\\Shoot\\In', 'C:\\Shoot\\Out'],
  ]);
  expect(movedPath('/job/Day 1', moves)).toBe('/job/Day 2');
  expect(movedPath('/job/Day 1/A.RAF', moves)).toBe('/job/Day 2/A.RAF');
  expect(movedPath('/job/Day 1/sub/A.RAF?vc=1', moves)).toBe('/job/Day 2/sub/A.RAF?vc=1');
  expect(movedPath('/job/Day 10/A.RAF', moves)).toBe('/job/Day 10/A.RAF');
  expect(movedPath('C:\\Shoot\\In\\A.NEF', moves)).toBe('C:\\Shoot\\Out\\A.NEF');
  expect(movedPath('Album: Day 1', moves)).toBe('Album: Day 1');
});

it('follows a move with the selection, the shown folder, pins and the open photograph', () => {
  const handleSettingsChange = vi.fn(() => Promise.resolve());
  useSettingsStore.setState({
    appSettings: { pinnedFolders: ['/job/Day 1/Best', '/other'], rootFolders: ['/job'] } as never,
    handleSettingsChange,
  });
  useLibraryStore.setState({
    currentFolderPath: '/job/Day 1',
    rootPaths: ['/job'],
    multiSelectedPaths: ['/job/Day 1/A.RAF', '/job/Day 1/A.RAF?vc=1', '/job/B.RAF'],
    libraryActivePath: '/job/Day 1/A.RAF?vc=1',
  });
  useEditorStore.setState({ selectedImage: { path: '/job/Day 1/A.RAF?vc=1' } as never });
  const { openPhoto } = followPathMoves([{ from: '/job/Day 1', to: '/job/Day 2' }]);
  expect(openPhoto).toBe('/job/Day 2/A.RAF?vc=1');
  const library = useLibraryStore.getState();
  expect(library.currentFolderPath).toBe('/job/Day 2');
  expect(library.rootPaths).toEqual(['/job']);
  expect(library.multiSelectedPaths).toEqual(['/job/Day 2/A.RAF', '/job/Day 2/A.RAF?vc=1', '/job/B.RAF']);
  expect(library.libraryActivePath).toBe('/job/Day 2/A.RAF?vc=1');
  expect(handleSettingsChange).toHaveBeenCalledWith(
    expect.objectContaining({ pinnedFolders: ['/job/Day 2/Best', '/other'], rootFolders: ['/job'] }),
  );

  // Nothing open moved: nothing to open again.
  useEditorStore.setState({ selectedImage: { path: '/job/B.RAF' } as never });
  expect(followPathMoves([{ from: '/job/Day 2', to: '/job/Day 1' }]).openPhoto).toBeNull();
  expect(useLibraryStore.getState().currentFolderPath).toBe('/job/Day 1');
});

it('reads the picks again from the catalog after paths changed', async () => {
  useLibraryStore.setState({ imageFlags: { '/job/Capture/A.RAF': 1 } });
  mockInvoke.mockResolvedValue({ '/job/Selects/A.RAF': 1 });
  await reloadSessionFlags();
  expect(mockInvoke).toHaveBeenCalledWith('session_flags', { directory: '/job' });
  expect(useLibraryStore.getState().imageFlags).toEqual({ '/job/Selects/A.RAF': 1 });
});

it('keeps a flag set while the picks are read, and ignores another session', async () => {
  useLibraryStore.setState({ imageFlags: { '/job/Capture/A.RAF': 1 } });
  let answer!: (flags: Record<string, number>) => void;
  mockInvoke.mockReturnValue(new Promise((resolve) => (answer = resolve)));
  const reload = reloadSessionFlags();
  // P/X pressed during the read: its acknowledged value is newer.
  useLibraryStore.setState({ imageFlags: { '/job/Capture/A.RAF': -1 } });
  answer({ '/job/Capture/A.RAF': 1, '/job/Capture/B.RAF': 1 });
  await reload;
  expect(useLibraryStore.getState().imageFlags).toEqual({ '/job/Capture/A.RAF': -1, '/job/Capture/B.RAF': 1 });

  mockInvoke.mockReturnValue(new Promise((resolve) => (answer = resolve)));
  const stale = reloadSessionFlags();
  useLibraryStore.setState({ sessionDirectory: '/other', imageFlags: {} });
  answer({ '/job/Capture/A.RAF': 1 });
  await stale;
  expect(useLibraryStore.getState().imageFlags).toEqual({});
});

it('does nothing outside a session', async () => {
  useLibraryStore.setState({ sessionDirectory: null });
  await reloadSessionFlags();
  expect(mockInvoke).not.toHaveBeenCalled();
});
