import { renderHook, waitFor } from '@testing-library/react';
import { useActivityStore } from '../store/useActivityStore';
import { expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { notify as toast } from '../activity/notify';
import { saveFailure, useLibraryActions } from './useLibraryActions';
import { useEditorStore } from '../store/useEditorStore';
import { useLibraryStore } from '../store/useLibraryStore';
import { useUIStore } from '../store/useUIStore';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn(() => Promise.resolve()) }));
vi.mock('../activity/notify', () => ({ notify: { error: vi.fn(), success: vi.fn(), info: vi.fn() } }));

it('reads which photos a batch save missed', () => {
  expect(saveFailure({ message: 'Could not save 1 of 2 photos', failed: ['/a.jpg'] }, ['/a.jpg', '/b.jpg'])).toEqual({
    message: 'Could not save 1 of 2 photos',
    failed: ['/a.jpg'],
  });
  expect(saveFailure('database is locked', ['/a.jpg', '/b.jpg'])).toEqual({
    message: 'database is locked',
    failed: ['/a.jpg', '/b.jpg'],
  });
});

it('a rating that was not saved shows the previous one again', async () => {
  vi.mocked(invoke).mockRejectedValueOnce({
    message: 'Could not save 1 of 2 photos: /b.jpg: database is locked',
    failed: ['/b.jpg'],
  });
  useEditorStore.setState({ selectedImage: null });
  useLibraryStore.setState({
    imageList: [],
    multiSelectedPaths: ['/a.jpg', '/b.jpg'],
    imageRatings: { '/a.jpg': 4, '/b.jpg': 4 },
  });
  const { result } = renderHook(() => useLibraryActions());
  result.current.handleRate(3);
  // Shown at once, before the save returns.
  expect(useLibraryStore.getState().imageRatings).toEqual({ '/a.jpg': 3, '/b.jpg': 3 });
  // Said under the activity centre's Needs attention, with the photograph.
  const failure = () => useActivityStore.getState().entries.find((entry) => entry.status === 'failure');
  await waitFor(() => expect(failure()).toBeTruthy());
  expect(useLibraryStore.getState().imageRatings).toEqual({ '/a.jpg': 3, '/b.jpg': 4 });
  expect(`${failure()!.title}\n${failure()!.detail ?? ''}`).toContain('/b.jpg: database is locked');
  expect(failure()!.paths).toEqual(['/b.jpg']);
});

it('rates nothing in the Library once the selection was emptied, the open photograph in Develop', () => {
  vi.mocked(invoke).mockClear();
  useEditorStore.setState({ selectedImage: { path: '/open.jpg' } as never });
  useLibraryStore.setState({ imageList: [], multiSelectedPaths: [], imageRatings: {} });
  useUIStore.setState({ activeView: 'library' });
  const { result } = renderHook(() => useLibraryActions());
  result.current.handleRate(2);
  expect(useLibraryStore.getState().imageRatings).toEqual({});
  expect(invoke).not.toHaveBeenCalled();
  useUIStore.setState({ activeView: 'editor' });
  result.current.handleRate(2);
  expect(useLibraryStore.getState().imageRatings).toEqual({ '/open.jpg': 2 });
});

it('labels nothing in the Library once the selection was emptied, the open photograph in Develop', async () => {
  vi.mocked(invoke).mockClear();
  useEditorStore.setState({ selectedImage: { path: '/open.jpg' } as never });
  useLibraryStore.setState({ imageList: [{ path: '/open.jpg', tags: [] } as never], multiSelectedPaths: [] });
  useUIStore.setState({ activeView: 'library' });
  const { result } = renderHook(() => useLibraryActions());
  await result.current.handleSetColorLabel('red');
  expect(invoke).not.toHaveBeenCalled();
  useUIStore.setState({ activeView: 'editor' });
  await result.current.handleSetColorLabel('red');
  expect(vi.mocked(invoke).mock.calls[0][1]).toEqual({ paths: ['/open.jpg'], color: 'red' });
  useUIStore.setState({ activeView: 'library' });
});
