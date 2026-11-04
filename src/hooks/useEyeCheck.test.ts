import '../i18n';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { act, cleanup, renderHook } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { BACKLOG_CHECK_MS, useEyeCheck } from './useEyeCheck';
import { useLibraryStore } from '../store/useLibraryStore';
import { useSettingsStore } from '../store/useSettingsStore';
import { Invokes, RawStatus, type AppSettings, type ImageFile } from '../components/ui/AppProperties';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(() => Promise.resolve(() => {})) }));
const { toast } = vi.hoisted(() => ({ toast: { warn: vi.fn(), error: vi.fn() } }));
vi.mock('../activity/notify', () => ({ notify: toast }));
const mockInvoke = vi.mocked(invoke);

const photo = (path: string) => ({ path }) as ImageFile;
const starts = () => mockInvoke.mock.calls.filter(([command]) => command === Invokes.StartEyeCheck).length;

beforeEach(() => {
  vi.useFakeTimers();
  mockInvoke.mockReset();
  mockInvoke.mockResolvedValue({});
  useLibraryStore.setState({
    sessionDirectory: '/A',
    imageList: [photo('/A/1.arw')],
    filterCriteria: { colors: [], rating: 0, rawStatus: RawStatus.All, eyes: 'closed' }
  });
  useSettingsStore.setState({ appSettings: { eyeCheck: true } as AppSettings });
});
afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

it('starts for the open library, again for photographs added later, and stops only its own pass', async () => {
  const { unmount } = renderHook(() => useEyeCheck());
  expect(mockInvoke).toHaveBeenCalledWith(Invokes.StartEyeCheck, { directory: '/A', download: false, resume: true });
  expect(starts()).toBe(1);
  // An import adds photographs after the pass finished: it is asked again.
  act(() => useLibraryStore.setState({ imageList: [photo('/A/1.arw'), photo('/A/2.arw')] }));
  await act(async () => vi.advanceTimersByTimeAsync(2500));
  expect(starts()).toBe(2);
  // An import into a folder not shown (the list stays as it is), tethering
  // or a hot folder: the library is asked again within BACKLOG_CHECK_MS.
  await act(async () => vi.advanceTimersByTimeAsync(BACKLOG_CHECK_MS));
  expect(starts()).toBe(3);
  // The looks for new photographs never lift a Cancel (`resume`).
  expect(mockInvoke).toHaveBeenLastCalledWith(Invokes.StartEyeCheck, {
    directory: '/A',
    download: false,
    resume: false
  });
  expect(mockInvoke).not.toHaveBeenCalledWith(Invokes.StopEyeCheck, expect.anything());
  // Another library: A's pass is stopped by name, B's started.
  act(() => useLibraryStore.setState({ sessionDirectory: '/B' }));
  expect(mockInvoke).toHaveBeenCalledWith(Invokes.StopEyeCheck, { directory: '/A' });
  expect(mockInvoke).toHaveBeenLastCalledWith(Invokes.StartEyeCheck, {
    directory: '/B',
    download: false,
    resume: true
  });
  unmount();
  expect(mockInvoke).toHaveBeenLastCalledWith(Invokes.StopEyeCheck, { directory: '/B' });
});

it('turned off, stops the pass and shows every photograph again', () => {
  renderHook(() => useEyeCheck());
  expect(useLibraryStore.getState().filterCriteria.eyes).toBe('closed');
  act(() => useSettingsStore.setState({ appSettings: { eyeCheck: false } as AppSettings }));
  expect(mockInvoke).toHaveBeenCalledWith(Invokes.StopEyeCheck, { directory: '/A' });
  expect(useLibraryStore.getState().filterCriteria.eyes).toBe('all');
});

it('keeps a saved eye filter while the settings are still loading', () => {
  useSettingsStore.setState({ appSettings: null });
  renderHook(() => useEyeCheck());
  expect(useLibraryStore.getState().filterCriteria.eyes).toBe('closed');
  expect(starts()).toBe(0);
});

it('offers the face models when they are missing, and shows any other failure', async () => {
  toast.warn.mockReset();
  mockInvoke.mockRejectedValueOnce({ code: 'models_missing', message: 'face-models-missing' });
  const { unmount } = renderHook(() => useEyeCheck());
  await act(async () => vi.advanceTimersByTimeAsync(0));
  expect(toast.warn).toHaveBeenCalledTimes(1);
  const [text, options] = toast.warn.mock.calls[0];
  expect(text).not.toContain('face-models-missing');
  expect(options.actions).toHaveLength(1);
  unmount();
  toast.warn.mockReset();
  mockInvoke.mockRejectedValueOnce({ code: 'internal', message: 'Disk full' });
  renderHook(() => useEyeCheck());
  await act(async () => vi.advanceTimersByTimeAsync(0));
  expect(toast.warn).toHaveBeenCalledTimes(1);
  expect(toast.warn.mock.calls[0][0]).toContain('Disk full');
  expect(toast.warn.mock.calls[0][1]).toBeUndefined();
});
