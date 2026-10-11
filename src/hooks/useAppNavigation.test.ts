import { act, cleanup, renderHook } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { AppNavigationProps, useAppNavigation } from './useAppNavigation';
import { useLibraryStore } from '../store/useLibraryStore';
import { useEditorStore } from '../store/useEditorStore';

const invoke = vi.hoisted(() => vi.fn());
vi.mock('@tauri-apps/api/core', () => ({ invoke }));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn() }));
vi.mock('@tauri-apps/api/path', () => ({ homeDir: vi.fn() }));
vi.mock('./useEditorActions', () => ({
  debouncedSave: { flush: vi.fn() }, debouncedSetHistory: { cancel: vi.fn() },
}));
const scans = new Map<string, (page: unknown) => void>();
// Every ref starts empty (null), including the boolean and number ones, so the fixture bridges via unknown.
const refs = () => Object.fromEntries([
  'transformWrapperRef', 'preloadedDataRef', 'cachedEditStateRef', 'selectedImagePathRef',
  'isBackendReadyRef', 'latestRenderedJobIdRef', 'previewJobIdRef', 'currentResRef', 'prevAdjustmentsRef',
].map((key) => [key, { current: null }])) as unknown as AppNavigationProps['refs'];
const file = (path: string) => ({ path, rating: 0, is_raw: true, tags: null, exif: null, modified: 0 });

beforeEach(() => {
  scans.clear();
  invoke.mockReset().mockImplementation((command, args) => {
    if (command === 'begin_image_listing') return new Promise((resolve) => scans.set(args.path, resolve));
    return Promise.resolve(undefined);
  });
  useLibraryStore.getState().setLibrary({ imageList: [], currentFolderPath: null, rootPaths: [], imageRatings: {} });
  useEditorStore.getState().setEditor({ selectedImage: null });
});
afterEach(cleanup);

it('does not let a previous folder response replace the newest folder or its loading state', async () => {
  const { result } = renderHook(() => useAppNavigation({ clearThumbnailQueue: vi.fn(), refs: refs() }));
  let old!: Promise<void>;
  let latest!: Promise<void>;
  await act(async () => { old = result.current.handleSelectSubfolder('/old'); });
  await act(async () => { latest = result.current.handleSelectSubfolder('/new'); });
  expect(scans.has('/old')).toBe(true);
  expect(scans.has('/new')).toBe(true);
  await act(async () => {
    scans.get('/old')!({ listing_id: 'old-snapshot', offset: 0, total: 700, files: [] });
    await old;
  });
  expect(useLibraryStore.getState().currentFolderPath).toBe('/new');
  expect(useLibraryStore.getState().isViewLoading).toBe(true);
  expect(useLibraryStore.getState().imageList).toEqual([]);
  expect(invoke).toHaveBeenCalledWith('cancel_image_listing', { listingId: 'old-snapshot' });
  await act(async () => {
    scans.get('/new')!({ listing_id: null, offset: 0, total: 1, files: [file('/new/photo.dng')] });
    await latest;
  });
  expect(useLibraryStore.getState().imageList.map((f) => f.path)).toEqual(['/new/photo.dng']);
  expect(useLibraryStore.getState().isViewLoading).toBe(false);
});

it('leaves Home empty when an outstanding folder scan finishes later', async () => {
  const { result } = renderHook(() => useAppNavigation({ clearThumbnailQueue: vi.fn(), refs: refs() }));
  let pending!: Promise<void>;
  await act(async () => { pending = result.current.handleSelectSubfolder('/old'); });
  act(() => result.current.handleGoHome());
  await act(async () => {
    scans.get('/old')!({ listing_id: null, offset: 0, total: 1, files: [file('/old/photo.dng')] });
    await pending;
  });
  expect(useLibraryStore.getState().imageList).toEqual([]);
  expect(useLibraryStore.getState().currentFolderPath).toBeNull();
  expect(useLibraryStore.getState().isViewLoading).toBe(false);
});

it('closes the previous photo when an album opens, so it is no longer the edit or export target', async () => {
  const { debouncedSave } = await import('./useEditorActions');
  invoke.mockImplementation((command) =>
    Promise.resolve(command === 'get_album_images' ? [file('/album/other.png')] : undefined),
  );
  useEditorStore.getState().setEditor({ selectedImage: { path: '/shoot/photo.dng' } as never });
  useLibraryStore.getState().setLibrary({
    multiSelectedPaths: ['/shoot/photo.dng'],
    libraryActivePath: '/shoot/photo.dng',
  });
  const { result } = renderHook(() => useAppNavigation({ clearThumbnailQueue: vi.fn(), refs: refs() }));
  await act(async () => {
    await result.current.handleSelectAlbum('a1', 'Others', ['/album/other.png']);
  });
  expect(debouncedSave.flush).toHaveBeenCalled();
  expect(useEditorStore.getState().selectedImage).toBeNull();
  expect(useLibraryStore.getState().multiSelectedPaths).toEqual([]);
  expect(useLibraryStore.getState().libraryActivePath).toBeNull();
  expect(useLibraryStore.getState().imageList.map((f) => f.path)).toEqual(['/album/other.png']);
});

it('keeps the photo in Develop when an album only refreshes in place', async () => {
  invoke.mockImplementation((command) =>
    Promise.resolve(command === 'get_album_images' ? [file('/album/other.png')] : undefined),
  );
  useEditorStore.getState().setEditor({ selectedImage: { path: '/album/other.png' } as never });
  const { result } = renderHook(() => useAppNavigation({ clearThumbnailQueue: vi.fn(), refs: refs() }));
  await act(async () => {
    await result.current.handleSelectAlbum('a1', 'Others', ['/album/other.png'], true);
  });
  expect(useEditorStore.getState().selectedImage?.path).toBe('/album/other.png');
});
