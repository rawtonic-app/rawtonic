import { act, cleanup, renderHook } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { debouncedSave, debouncedSetHistory, useEditorActions } from './useEditorActions';
import { useAppNavigation, type AppNavigationProps } from './useAppNavigation';
import { useKeyboardShortcuts } from './useKeyboardShortcuts';
import { useEditorStore } from '../store/useEditorStore';
import { useLibraryStore } from '../store/useLibraryStore';
import { useSettingsStore } from '../store/useSettingsStore';
import { useUIStore } from '../store/useUIStore';
import { INITIAL_ADJUSTMENTS } from '../utils/adjustments';
import { Invokes, type AppSettings, type ImageFile } from '../components/ui/AppProperties';

// The loupe and the full screen are views (user decision 2026-10-25): an
// edit under way when F is pressed completes. It must land on the
// photograph it was made on, also when the view has stepped on meanwhile.

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn() }));
vi.mock('@tauri-apps/api/path', () => ({ homeDir: vi.fn() }));
vi.mock('../activity/notify', () => ({ notify: { error: vi.fn(), warn: vi.fn(), success: vi.fn() } }));
vi.mock('../utils/photoFullScreen', () => ({
  followPhotoFullScreen: vi.fn(() => Promise.resolve()),
  toggleWindowFullScreen: vi.fn(() => Promise.resolve()),
}));

const files = ['/s/a.dng', '/s/b.dng'].map((path) => ({ path }) as ImageFile);
let autoAnswer: ((value: unknown) => void) | null;
const refs = () =>
  Object.fromEntries(
    [
      'transformWrapperRef', 'cachedEditStateRef', 'selectedImagePathRef', 'isBackendReadyRef',
      'latestRenderedJobIdRef', 'previewJobIdRef', 'currentResRef', 'prevAdjustmentsRef',
    ].map((key) => [key, { current: null }]),
  ) as unknown as AppNavigationProps['refs'];
const press = (code: string, key = code) =>
  act(() => {
    window.dispatchEvent(new KeyboardEvent('keydown', { code, key, bubbles: true, cancelable: true }));
  });
const saves = () =>
  vi.mocked(invoke).mock.calls.filter(([command]) => command === Invokes.SaveMetadataAndUpdateThumbnail);

beforeEach(() => {
  autoAnswer = null;
  vi.mocked(invoke).mockImplementation((command) =>
    command === Invokes.CalculateAutoAdjustments
      ? new Promise((resolve) => (autoAnswer = resolve))
      : Promise.resolve(undefined as never),
  );
  useSettingsStore.setState({ appSettings: { keybinds: {} } as unknown as AppSettings, osPlatform: 'macos' });
  useLibraryStore.setState({ imageList: files, multiSelectedPaths: ['/s/a.dng'], libraryActivePath: '/s/a.dng' });
  useUIStore.setState({ activeView: 'editor', isFullScreen: false, libraryLoupe: false, isSettingsOpen: false });
  const adjustments = { ...INITIAL_ADJUSTMENTS, exposure: -2 };
  useEditorStore.setState({
    selectedImage: { path: '/s/a.dng', isReady: true, isRaw: true, width: 40, height: 30, exif: {}, thumbnailUrl: '' } as never,
    adjustments,
    history: [adjustments],
    historyIndex: 0,
  });
});
afterEach(() => {
  cleanup();
  debouncedSave.cancel();
  debouncedSetHistory.cancel();
  useEditorStore.setState({ selectedImage: null });
  vi.clearAllMocks();
});

function mount() {
  return renderHook(() => {
    const navigation = useAppNavigation({ clearThumbnailQueue: vi.fn(), refs: refs() });
    const actions = useEditorActions();
    useKeyboardShortcuts({
      sortedImageList: files,
      handleBackToLibrary: navigation.handleBackToLibrary,
      handleImageSelect: navigation.handleImageSelect,
      handleUndoFileOperation: vi.fn(),
      handleDeleteSelected: vi.fn(),
      handlePasteFiles: vi.fn(),
      handleZoomChange: vi.fn(),
    } as never);
    return actions;
  });
}

it('saves the edit made before F to its own photograph when the full screen steps to the next', async () => {
  mount();
  // The edit's save is still waiting (debounced) as F is pressed.
  debouncedSave('/s/a.dng', { ...INITIAL_ADJUSTMENTS, exposure: 1 });
  press('KeyF', 'f');
  expect(useUIStore.getState().isFullScreen).toBe(true);
  press('ArrowRight');
  await act(async () => {});
  expect(useEditorStore.getState().selectedImage?.path).toBe('/s/b.dng');
  expect(saves()).toEqual([
    [Invokes.SaveMetadataAndUpdateThumbnail, { path: '/s/a.dng', adjustments: expect.objectContaining({ exposure: 1 }) }],
  ]);
});

it('never puts an answer asked for the previous photograph on the one the full screen shows now', async () => {
  const { result } = mount();
  act(() => {
    void result.current.handleAutoAdjustments();
  });
  press('KeyF', 'f');
  press('ArrowRight');
  await act(async () => {});
  expect(useEditorStore.getState().selectedImage?.path).toBe('/s/b.dng');
  // The next photograph has loaded before the answer comes back.
  const before = { ...INITIAL_ADJUSTMENTS, exposure: 0.5 };
  act(() =>
    useEditorStore.setState((s) => ({ selectedImage: { ...s.selectedImage!, isReady: true }, adjustments: before })),
  );
  await act(async () => autoAnswer!({ ...INITIAL_ADJUSTMENTS, exposure: 4 }));
  expect(useEditorStore.getState().adjustments).toBe(before);
  expect(useEditorStore.getState().adjustments.exposure).not.toBe(4);
  await act(async () => debouncedSave.flush());
  expect(saves().some(([, args]) => (args as { path: string }).path === '/s/b.dng')).toBe(false);
});

it('completes an edit under way when F is pressed on the same photograph', async () => {
  const { result } = mount();
  act(() => {
    void result.current.handleAutoAdjustments();
  });
  press('KeyF', 'f');
  await act(async () => autoAnswer!({ ...INITIAL_ADJUSTMENTS, exposure: 4 }));
  expect(useEditorStore.getState()).toMatchObject({ selectedImage: { path: '/s/a.dng' } });
  expect(useEditorStore.getState().adjustments.exposure).toBe(4);
});
