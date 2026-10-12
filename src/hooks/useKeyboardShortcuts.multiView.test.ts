import { cleanup, renderHook } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { useKeyboardShortcuts } from './useKeyboardShortcuts';
import { useLibraryStore } from '../store/useLibraryStore';
import { useSettingsStore } from '../store/useSettingsStore';
import { useUIStore } from '../store/useUIStore';
import { useEditorStore } from '../store/useEditorStore';
import { useMultiViewStore } from '../store/useMultiViewStore';
import { Invokes, type AppSettings, type ImageFile } from '../components/ui/AppProperties';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn(() => Promise.resolve()) }));
vi.mock('react-toastify', () => ({ toast: { error: vi.fn(), success: vi.fn() } }));

const list = ['/s/a.CR3', '/s/b.CR3', '/s/c.CR3', '/s/d.CR3', '/s/e.CR3'].map(
  (path) => ({ path }) as ImageFile,
);
const props = {
  sortedImageList: list,
  handleBackToLibrary: vi.fn(),
  handleDeleteSelected: vi.fn(),
  handleImageSelect: vi.fn(),
  handlePasteFiles: vi.fn(),
  handleZoomChange: vi.fn(),
};
const press = (code: string, init: KeyboardEventInit = {}) =>
  window.dispatchEvent(new KeyboardEvent('keydown', { code, bubbles: true, cancelable: true, ...init }));

function develop(primary: string, selection: string[], active: boolean) {
  useLibraryStore.setState({ multiSelectedPaths: selection, imageRatings: {} });
  useEditorStore.setState({ selectedImage: { path: primary, isReady: true } as never });
  useMultiViewStore.setState({ active, enabled: true, unavailable: false });
}

beforeEach(() => {
  vi.clearAllMocks();
  useSettingsStore.setState({
    appSettings: { keybinds: {} } as unknown as AppSettings,
    osPlatform: 'macos',
  });
  useUIStore.setState({ activeView: 'editor', isSettingsOpen: false });
  renderHook(() => useKeyboardShortcuts(props));
});
afterEach(() => cleanup());

describe('multi view keys', () => {
  it('N switches the multi view on and off (remembered)', () => {
    develop('/s/b.CR3', ['/s/b.CR3', '/s/d.CR3'], true);
    press('KeyN');
    expect(useMultiViewStore.getState().enabled).toBe(false);
    expect(localStorage.getItem('rawtonic.develop.multiView')).toBe('off');
    press('KeyN');
    expect(useMultiViewStore.getState().enabled).toBe(true);
  });

  it('N does nothing without the native display', () => {
    useSettingsStore.setState({ osPlatform: 'linux' });
    develop('/s/b.CR3', ['/s/b.CR3', '/s/d.CR3'], false);
    press('KeyN');
    expect(useMultiViewStore.getState().enabled).toBe(true);
  });

  it('the arrows step the primary within the selection and wrap', () => {
    develop('/s/d.CR3', ['/s/d.CR3', '/s/b.CR3'], true);
    press('ArrowRight');
    expect(props.handleImageSelect).toHaveBeenLastCalledWith('/s/b.CR3', true);
    develop('/s/b.CR3', ['/s/d.CR3', '/s/b.CR3'], true);
    press('ArrowLeft');
    // b is first in browser order: wraps to d.
    expect(props.handleImageSelect).toHaveBeenLastCalledWith('/s/d.CR3', true);
  });

  it('Cmd+arrows step the primary too', () => {
    develop('/s/b.CR3', ['/s/b.CR3', '/s/c.CR3', '/s/e.CR3'], true);
    press('ArrowRight', { metaKey: true });
    expect(props.handleImageSelect).toHaveBeenLastCalledWith('/s/c.CR3', true);
    press('ArrowLeft', { metaKey: true });
    expect(props.handleImageSelect).toHaveBeenLastCalledWith('/s/e.CR3', true);
  });

  it('in the single view the arrows still go through the folder', () => {
    develop('/s/b.CR3', ['/s/b.CR3', '/s/d.CR3'], false);
    press('ArrowRight');
    expect(props.handleImageSelect).toHaveBeenLastCalledWith('/s/c.CR3', true);
  });

  it('rating keys rate the primary only', async () => {
    develop('/s/b.CR3', ['/s/b.CR3', '/s/d.CR3'], true);
    press('Digit3');
    expect(useLibraryStore.getState().imageRatings).toEqual({ '/s/b.CR3': 3 });
    await vi.waitFor(() =>
      expect(vi.mocked(invoke)).toHaveBeenCalledWith(Invokes.SetRatingForPaths, {
        paths: ['/s/b.CR3'],
        rating: 3,
      }),
    );
  });

  it('zooming in returns to the single view first; zooming out is ignored', () => {
    develop('/s/b.CR3', ['/s/b.CR3', '/s/d.CR3'], true);
    press('Minus', { metaKey: true });
    expect(useMultiViewStore.getState().enabled).toBe(true);
    expect(props.handleZoomChange).not.toHaveBeenCalled();
    vi.useFakeTimers();
    press('Digit1', { metaKey: true });
    expect(useMultiViewStore.getState().enabled).toBe(false);
    vi.advanceTimersByTime(400);
    vi.useRealTimers();
    expect(props.handleZoomChange).toHaveBeenCalledWith(1.0);
  });
});
