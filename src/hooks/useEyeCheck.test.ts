import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { act, cleanup, renderHook } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { useEyeCheck } from './useEyeCheck';
import { useLibraryStore } from '../store/useLibraryStore';
import { useSettingsStore } from '../store/useSettingsStore';
import { Invokes, RawStatus, type AppSettings, type ImageFile } from '../components/ui/AppProperties';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(() => Promise.resolve(() => {})) }));
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
    filterCriteria: { colors: [], rating: 0, rawStatus: RawStatus.All, eyes: 'closed' },
  });
  useSettingsStore.setState({ appSettings: { eyeCheck: true } as AppSettings });
});
afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

it('starts for the open library, again for photographs added later, and stops only its own pass', async () => {
  const { unmount } = renderHook(() => useEyeCheck());
  expect(mockInvoke).toHaveBeenCalledWith(Invokes.StartEyeCheck, { directory: '/A', download: false });
  expect(starts()).toBe(1);
  // An import adds photographs after the pass finished: it is asked again.
  act(() => useLibraryStore.setState({ imageList: [photo('/A/1.arw'), photo('/A/2.arw')] }));
  await act(async () => vi.advanceTimersByTimeAsync(2500));
  expect(starts()).toBe(2);
  expect(mockInvoke).not.toHaveBeenCalledWith(Invokes.StopEyeCheck, expect.anything());
  // Another library: A's pass is stopped by name, B's started.
  act(() => useLibraryStore.setState({ sessionDirectory: '/B' }));
  expect(mockInvoke).toHaveBeenCalledWith(Invokes.StopEyeCheck, { directory: '/A' });
  expect(mockInvoke).toHaveBeenLastCalledWith(Invokes.StartEyeCheck, { directory: '/B', download: false });
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
