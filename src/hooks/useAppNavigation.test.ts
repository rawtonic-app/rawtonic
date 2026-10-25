import { act, cleanup, renderHook } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { AppNavigationProps, useAppNavigation } from './useAppNavigation';
import { useLibraryStore } from '../store/useLibraryStore';
import { useEditorStore } from '../store/useEditorStore';

const invoke = vi.hoisted(() => vi.fn());
vi.mock('@tauri-apps/api/core', () => ({ invoke }));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn() }));
vi.mock('@tauri-apps/api/path', () => ({ homeDir: vi.fn() }));
const history = vi.hoisted(() => ({ cancel: vi.fn(), flush: vi.fn() }));
vi.mock('./useEditorActions', () => ({
  debouncedSave: { flush: vi.fn() }, debouncedSetHistory: history,
}));
const scans = new Map<string, (page: unknown) => void>();
// Every ref starts empty (null), including the boolean and number ones, so the fixture bridges via unknown.
const refs = () => Object.fromEntries([
  'transformWrapperRef', 'cachedEditStateRef', 'selectedImagePathRef',
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

it('keeps the last edit in the history when going back to the Library', () => {
  history.cancel.mockClear();
  history.flush.mockClear();
  useEditorStore.getState().setEditor({ selectedImage: { path: '/a.dng', isReady: true } as never });
  const { result } = renderHook(() => useAppNavigation({ clearThumbnailQueue: vi.fn(), refs: refs() }));
  act(() => result.current.handleBackToLibrary());
  expect(history.flush).toHaveBeenCalled();
  expect(history.cancel).not.toHaveBeenCalled();
});

it('finds the root folder a path is in, not one that merely shares its name prefix', async () => {
  const { rootContaining } = await import('./useAppNavigation');
  expect(rootContaining('/photos/2024-old/a', ['/photos/2024', '/photos/2024-old'])).toBe('/photos/2024-old');
  expect(rootContaining('/photos/2024-old/a', ['/photos/2024'])).toBeUndefined();
  expect(rootContaining('/photos/2024/a', ['/photos', '/photos/2024'])).toBe('/photos/2024');
  expect(rootContaining('/photos/2024', ['/photos/2024'])).toBe('/photos/2024');
  expect(rootContaining('D:\\Shoots\\Day 1', ['D:\\Shoots'])).toBe('D:\\Shoots');
});

describe('stepping to a photograph without a preview proxy', () => {
  const stepFromReady = async (activation: string) => {
    const { useUIStore } = await import('../store/useUIStore');
    useUIStore.getState().setUI({ activeView: 'editor' });
    invoke.mockImplementation((command) =>
      Promise.resolve(command === 'activate_photo' ? activation : undefined),
    );
    useEditorStore.getState().setEditor({
      selectedImage: { path: '/s/a.NEF', isReady: true } as never,
      hasRenderedFirstFrame: true,
      stillPreview: null,
    });
    const { result } = renderHook(() => useAppNavigation({ clearThumbnailQueue: vi.fn(), refs: refs() }));
    await act(async () => {
      await result.current.handleImageSelect('/s/b.NEF', true);
    });
  };
  afterEach(() => vi.useRealTimers());

  it('marks its thumbnail on screen as a preview and keeps it until the developed frame', async () => {
    vi.useFakeTimers({ toFake: ['setTimeout'] });
    await stepFromReady('still');
    expect(useEditorStore.getState().stillPreview).toBe('/s/b.NEF');
    expect(useEditorStore.getState().holdingNativeFrame).toBe(true);
    // The hold that would let the webview cover the previous photograph's
    // frame does not end while the still of this one is shown.
    act(() => vi.advanceTimersByTime(5000));
    expect(useEditorStore.getState().holdingNativeFrame).toBe(true);
    // The first developed frame replaces it in place (useTauriListeners'
    // wgpu-frame-ready).
    act(() => useEditorStore.getState().setEditor({ hasRenderedFirstFrame: true, holdingNativeFrame: false, stillPreview: null }));
    expect(useEditorStore.getState().stillPreview).toBeNull();
  });

  it('marks nothing when a developed frame from its proxy is on the way', async () => {
    vi.useFakeTimers({ toFake: ['setTimeout'] });
    await stepFromReady('developed');
    expect(useEditorStore.getState().stillPreview).toBeNull();
    act(() => vi.advanceTimersByTime(5000));
    expect(useEditorStore.getState().holdingNativeFrame).toBe(false);
  });

  it('never marks a still that arrives after the developed frame', async () => {
    let answer!: (value: string) => void;
    const { useUIStore } = await import('../store/useUIStore');
    useUIStore.getState().setUI({ activeView: 'editor' });
    invoke.mockImplementation((command) =>
      command === 'activate_photo' ? new Promise((resolve) => { answer = resolve; }) : Promise.resolve(undefined),
    );
    useEditorStore.getState().setEditor({
      selectedImage: { path: '/s/a.NEF', isReady: true } as never,
      hasRenderedFirstFrame: true,
      stillPreview: null,
    });
    const { result } = renderHook(() => useAppNavigation({ clearThumbnailQueue: vi.fn(), refs: refs() }));
    await act(async () => {
      await result.current.handleImageSelect('/s/b.NEF', true);
    });
    act(() => useEditorStore.getState().setEditor({ hasRenderedFirstFrame: true, holdingNativeFrame: false }));
    await act(async () => answer('still'));
    expect(useEditorStore.getState().stillPreview).toBeNull();
    expect(useEditorStore.getState().holdingNativeFrame).toBe(false);
  });
});
