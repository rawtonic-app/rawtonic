import '../i18n';
import { afterEach, expect, it, vi } from 'vitest';
import { renderHook } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { useLibraryStore } from '../store/useLibraryStore';
import { useEditorStore } from '../store/useEditorStore';
import { useUIStore } from '../store/useUIStore';
import { runAppCommand } from '../utils/appCommands';
import { buildComboMap } from '../utils/keyboardUtils';
import { useVirtualCopyCommand } from './useVirtualCopyCommand';
import { notify } from '../activity/notify';
import type { ImageFile } from '../components/ui/AppProperties';

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(async (command: string) => (command === 'get_albums' ? [{ id: 'album', name: 'A' }] : undefined))
}));
vi.mock('../activity/notify', () => ({ notify: { error: vi.fn(), success: vi.fn() } }));
const { flush } = vi.hoisted(() => ({ flush: vi.fn(async () => [] as string[]) }));
vi.mock('../utils/quit', () => ({ flushBeforeQuit: flush, registerQuitFlush: vi.fn(() => vi.fn()) }));

afterEach(() => vi.mocked(invoke).mockClear());

// UXR3: a virtual copy took three actions (right-click, Duplicate photo,
// Virtual copy); Lightroom makes one with ⌘' and Capture One with a key too.
it("makes a virtual copy of the one selected or open photo with ⌘'", async () => {
  expect(buildComboMap(undefined).get('ctrl+Quote')).toContain('virtual_copy');
  const refresh = vi.fn(async () => {});
  const hook = renderHook(() => useVirtualCopyCommand(refresh));
  useUIStore.setState({ activeView: 'library' });
  useLibraryStore.setState({ multiSelectedPaths: ['/job/a.CR3', '/job/b.CR3'], activeAlbumId: null });
  expect(runAppCommand('virtual_copy')).toBe(false); // two photos
  useLibraryStore.setState({ multiSelectedPaths: ['/job/a.CR3'] });
  expect(runAppCommand('virtual_copy')).toBe(true);
  await vi.waitFor(() => expect(refresh).toHaveBeenCalledTimes(1));
  expect(invoke).toHaveBeenCalledWith('create_virtual_copy', { sourceVirtualPath: '/job/a.CR3', targetAlbumId: null });
  // In Develop: the open photo.
  useUIStore.setState({ activeView: 'editor' });
  useEditorStore.setState({ selectedImage: { path: '/job/c.CR3', isReady: true } as never });
  expect(runAppCommand('virtual_copy')).toBe(true);
  await vi.waitFor(() =>
    expect(invoke).toHaveBeenCalledWith('create_virtual_copy', { sourceVirtualPath: '/job/c.CR3', targetAlbumId: null })
  );
  hook.unmount();
  expect(runAppCommand('virtual_copy')).toBe(false);
});

// Review round 1: edits still waiting for their save are written before
// the copy reads the recipe, and in an album the album's membership is
// read again before the list, so the new copy shows there.
it('saves pending edits first and reloads the album the copy joins', async () => {
  const refresh = vi.fn(async () => {});
  const hook = renderHook(() => useVirtualCopyCommand(refresh));
  useUIStore.setState({ activeView: 'library' });
  useLibraryStore.setState({ multiSelectedPaths: ['/job/a.CR3'], activeAlbumId: 'album', albumTree: [] });
  const order: string[] = [];
  flush.mockImplementationOnce(async () => {
    order.push('flush');
    return [];
  });
  vi.mocked(invoke).mockImplementation(async (command: string) => {
    order.push(command);
    return command === 'get_albums' ? [{ id: 'album', name: 'A' }] : undefined;
  });
  refresh.mockImplementationOnce(async () => {
    order.push('refresh');
  });
  expect(runAppCommand('virtual_copy')).toBe(true);
  await vi.waitFor(() => expect(refresh).toHaveBeenCalled());
  expect(order).toEqual(['flush', 'create_virtual_copy', 'get_albums', 'refresh']);
  expect(useLibraryStore.getState().albumTree).toEqual([{ id: 'album', name: 'A' }]);
  // An edit that cannot be saved: no copy of an older recipe.
  flush.mockImplementationOnce(async () => ['Edits of a.CR3']);
  vi.mocked(invoke).mockClear();
  expect(runAppCommand('virtual_copy')).toBe(true);
  await vi.waitFor(() => expect(flush).toHaveBeenCalledTimes(2));
  await new Promise((resolve) => setTimeout(resolve, 0));
  expect(invoke).not.toHaveBeenCalledWith('create_virtual_copy', expect.anything());
  hook.unmount();
});

// UXR4M-19: the copy was made but nothing showed it, so the user pressed
// again (four hidden copies in one run). Lightroom shows the new copy
// selected beside its photo; Develop switches to it.
it('selects the new copy, opens it where a photo is shown and names it', async () => {
  const copy = '/job/a.CR3?vc=abc123';
  vi.mocked(invoke).mockImplementation(async (command: string) =>
    command === 'create_virtual_copy' ? copy : undefined
  );
  const refresh = vi.fn(async () => {
    useLibraryStore.setState({
      imageList: [{ path: '/job/a.CR3' }, { path: copy }, { path: '/job/b.CR3' }] as ImageFile[]
    });
  });
  const open = vi.fn();
  const hook = renderHook(() => useVirtualCopyCommand(refresh, open));
  useUIStore.setState({ activeView: 'library', libraryLoupe: false, isFullScreen: false });
  useLibraryStore.setState({
    multiSelectedPaths: ['/job/a.CR3'],
    libraryActivePath: '/job/a.CR3',
    activeAlbumId: null
  });
  expect(runAppCommand('virtual_copy')).toBe(true);
  await vi.waitFor(() => expect(notify.success).toHaveBeenCalledWith('Made virtual copy 1 of a.CR3.'));
  expect(useLibraryStore.getState().multiSelectedPaths).toEqual([copy]);
  expect(useLibraryStore.getState().libraryActivePath).toBe(copy);
  expect(open).not.toHaveBeenCalled();
  // In Develop the copy opens there.
  useUIStore.setState({ activeView: 'editor' });
  useEditorStore.setState({ selectedImage: { path: '/job/a.CR3', isReady: true } as never });
  expect(runAppCommand('virtual_copy')).toBe(true);
  await vi.waitFor(() => expect(open).toHaveBeenCalledWith(copy, true));
  hook.unmount();
});

it('says so when the view does not show the new copy', async () => {
  vi.mocked(invoke).mockImplementation(async (command: string) =>
    command === 'create_virtual_copy' ? '/job/a.CR3?vc=def456' : undefined
  );
  vi.mocked(notify.success).mockClear();
  const refresh = vi.fn(async () => {
    useLibraryStore.setState({ imageList: [{ path: '/job/a.CR3' }] as ImageFile[] });
  });
  const hook = renderHook(() => useVirtualCopyCommand(refresh));
  useUIStore.setState({ activeView: 'library' });
  useLibraryStore.setState({ multiSelectedPaths: ['/job/a.CR3'], activeAlbumId: null });
  expect(runAppCommand('virtual_copy')).toBe(true);
  await vi.waitFor(() =>
    expect(notify.success).toHaveBeenCalledWith(
      "Made a virtual copy of a.CR3. This view does not show it; the photo's folder does."
    )
  );
  expect(useLibraryStore.getState().multiSelectedPaths).toEqual(['/job/a.CR3']);
  hook.unmount();
});
