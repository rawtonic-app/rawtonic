import { createElement } from 'react';
import { act, cleanup, render, renderHook, screen, within } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';

const { invoke, exit } = vi.hoisted(() => ({ invoke: vi.fn(), exit: vi.fn() }));
vi.mock('@tauri-apps/api/core', () => ({ invoke }));
vi.mock('@tauri-apps/plugin-process', () => ({ exit }));

import ActivityCentre from '../components/activity/ActivityCentre';
import '../i18n';

import { useExternalEditSession } from './useExternalEditSession';
import { useProcessStore, type ExportJob } from '../store/useProcessStore';
import { useEditorStore } from '../store/useEditorStore';
import { useActivityStore } from '../store/useActivityStore';
import { waitFor } from '@testing-library/react';

afterEach(cleanup);

const job = (id: string, state: ExportJob['state']): ExportJob => ({
  id,
  label: id,
  state,
  createdAt: '',
  total: 1,
  done: state === 'done' ? 1 : 0,
  failed: state === 'failed' ? 1 : 0,
  failures: []
});

beforeEach(() => {
  invoke.mockReset();
  exit.mockReset();
  invoke.mockResolvedValue('mine');
  useProcessStore.setState({
    exportQueue: [],
    externalEditSession: { source: '/a.dng', output: '/out/a.tif', format: 'tif', jpegQuality: 90 } as never
  });
  useEditorStore.setState({ selectedImage: { path: '/a.dng', isReady: true } as never });
});

it('quits only when its own export is done, not when a batch queued before it completes', async () => {
  const { result } = renderHook(() => useExternalEditSession(vi.fn()));
  await act(() => result.current.finishExternalEdit());
  // The batch ahead of it finishes first.
  act(() => useProcessStore.setState({ exportQueue: [job('batch', 'done'), job('mine', 'queued')] }));
  act(() => useProcessStore.getState().setExportState({ status: 'success' as never }));
  expect(exit).not.toHaveBeenCalled();
  expect(result.current.isFinishing).toBe(true);
  act(() => useProcessStore.setState({ exportQueue: [job('batch', 'done'), job('mine', 'done')] }));
  expect(exit).toHaveBeenCalledWith(0);
});

it('stays open when its export fails', async () => {
  const { result } = renderHook(() => useExternalEditSession(vi.fn()));
  await act(() => result.current.finishExternalEdit());
  act(() => useProcessStore.setState({ exportQueue: [job('mine', 'failed')] }));
  expect(exit).not.toHaveBeenCalled();
  expect(result.current.isFinishing).toBe(false);
});

it('can be pressed again when its waiting export was removed from the queue', async () => {
  const { result } = renderHook(() => useExternalEditSession(vi.fn()));
  await act(() => result.current.finishExternalEdit());
  // Not listed yet: still waiting.
  expect(result.current.isFinishing).toBe(true);
  act(() => useProcessStore.setState({ exportQueue: [job('batch', 'running'), job('mine', 'queued')] }));
  expect(result.current.isFinishing).toBe(true);
  act(() => useProcessStore.setState({ exportQueue: [job('batch', 'running')] }));
  expect(result.current.isFinishing).toBe(false);
  expect(exit).not.toHaveBeenCalled();
});

it('keeps a launch photo queued until remembered-library navigation has finished', async () => {
  useProcessStore.setState({
    externalEditSession: null,
    startupLibraryReady: false,
    initialFileToOpen: '/outside/photo.ARW'
  });
  invoke.mockImplementation(async (command: string) =>
    command === 'launch_path_library' ? null : command === 'launch_path_is_photo'
  );
  const select = vi.fn();
  renderHook(() => useExternalEditSession(select));
  expect(select).not.toHaveBeenCalled();
  expect(useProcessStore.getState().initialFileToOpen).toBe('/outside/photo.ARW');
  await act(async () => useProcessStore.setState({ startupLibraryReady: true }));
  expect(select).toHaveBeenCalledExactlyOnceWith('/outside/photo.ARW');
  expect(useProcessStore.getState().initialFileToOpen).toBeNull();
});

it('browses a folder launch without selecting an image or importing', async () => {
  invoke.mockImplementation(async (command: string) => (command === 'launch_path_library' ? null : true));
  useProcessStore.setState({ externalEditSession: null, startupLibraryReady: true, initialFileToOpen: '/photos' });
  const select = vi.fn();
  const browse = vi.fn();
  await act(async () => {
    renderHook(() => useExternalEditSession(select, browse));
  });
  expect(browse).toHaveBeenCalledExactlyOnceWith('/photos');
  expect(select).not.toHaveBeenCalled();
});

it('opens a library the launch names as that library, not as a folder', async () => {
  invoke.mockImplementation(async (command: string) => (command === 'launch_path_library' ? '/lib/Wedding' : true));
  useProcessStore.setState({
    externalEditSession: null,
    startupLibraryReady: true,
    initialFileToOpen: '/lib/Wedding/catalog.sqlite',
    libraryToOpen: null
  });
  const select = vi.fn();
  const browse = vi.fn();
  await act(async () => {
    renderHook(() => useExternalEditSession(select, browse));
  });
  expect(useProcessStore.getState().libraryToOpen).toBe('/lib/Wedding');
  expect(browse).not.toHaveBeenCalled();
  expect(select).not.toHaveBeenCalled();
});

// macOS lists Rawtonic for every camera RAW type it knows (tauri.macos.conf.json),
// some of which Rawtonic cannot read yet (Nikon NEFX, Sony AXR, …); the backend
// says which files are photos it can show (formats::is_openable_photo).
it.each([
  ['/photos/merged.NEFX', false],
  ['/photos/shot.nef', true]
])('a file handed over by Open With (%s) opens only when the app can show it', async (path, photo) => {
  invoke.mockReset();
  invoke.mockImplementation(async (command: string) =>
    command === 'launch_path_library' ? null : command === 'launch_path_is_photo' ? photo : false
  );
  useActivityStore.setState({ entries: [], open: false });
  useProcessStore.setState({ externalEditSession: null, startupLibraryReady: true, initialFileToOpen: path } as never);
  const select = vi.fn();
  renderHook(() => useExternalEditSession(select));
  if (photo) {
    await waitFor(() => expect(select).toHaveBeenCalledWith(path));
    expect(useActivityStore.getState().entries).toEqual([]);
  } else {
    render(createElement(ActivityCentre));
    // Assert the actual notice, including the basename and support explanation.
    const dialog = await screen.findByRole('dialog', { name: 'Activity' });
    expect(
      within(dialog).getByText('merged.NEFX cannot be opened: Rawtonic does not support this file format yet.')
    ).toBeTruthy();
    // The activity centre opens on a failure naming the file.
    await waitFor(() => expect(useActivityStore.getState().open).toBe(true));
    const [entry] = useActivityStore.getState().entries;
    expect(entry.status).toBe('failure');
    expect(entry.detail).toBe(path);
    // No Show photos action for the file it refused.
    expect(entry.paths ?? []).toEqual([]);
    expect(select).not.toHaveBeenCalled();
  }
});
