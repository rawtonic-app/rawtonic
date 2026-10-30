import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { Invokes } from '../components/ui/AppProperties';
import { useLibraryStore } from '../store/useLibraryStore';
import { watchXmpSidecars } from './xmpCheck';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));

const checks = () =>
  vi.mocked(invoke).mock.calls.filter(([command]) => command === Invokes.SessionXmpCheck).map(([, args]) => args);

beforeEach(() => {
  vi.useFakeTimers();
  useLibraryStore.setState({ sessionDirectory: '/lib', currentFolderPath: null });
  vi.mocked(invoke).mockResolvedValue(0);
});
afterEach(() => {
  vi.useRealTimers();
  vi.clearAllMocks();
});

it('checks the whole library when it opens and says when another program changed a sidecar', async () => {
  vi.mocked(invoke).mockResolvedValueOnce(2);
  const found = vi.fn();
  const stop = watchXmpSidecars('/lib', found);
  await vi.advanceTimersByTimeAsync(0);
  expect(checks()).toEqual([{ directory: '/lib', folder: null }]);
  expect(found).toHaveBeenCalledTimes(1);
  stop();
});

it('checks again when the app is back in front, without any write', async () => {
  const found = vi.fn();
  const stop = watchXmpSidecars('/lib', found);
  await vi.advanceTimersByTimeAsync(0);
  expect(found).not.toHaveBeenCalled();
  // Rated in another program meanwhile.
  vi.mocked(invoke).mockResolvedValueOnce(1);
  await vi.advanceTimersByTimeAsync(5000);
  window.dispatchEvent(new Event('focus'));
  await vi.advanceTimersByTimeAsync(0);
  expect(checks()).toHaveLength(2);
  expect(found).toHaveBeenCalledTimes(1);
  // A burst of focus changes is one check a second at most.
  for (let i = 0; i < 5; i++) window.dispatchEvent(new Event('focus'));
  await vi.advanceTimersByTimeAsync(1500);
  expect(checks()).toHaveLength(3);
  stop();
});

it('checks a folder when it is shown, not a view that is no folder', async () => {
  const stop = watchXmpSidecars('/lib', vi.fn());
  await vi.advanceTimersByTimeAsync(0);
  useLibraryStore.setState({ currentFolderPath: '/lib/Capture' });
  await vi.advanceTimersByTimeAsync(0);
  expect(checks()).toEqual([
    { directory: '/lib', folder: null },
    { directory: '/lib', folder: '/lib/Capture' },
  ]);
  useLibraryStore.setState({ currentFolderPath: 'Album: Picks' });
  await vi.advanceTimersByTimeAsync(2000);
  expect(checks()).toHaveLength(2);
  stop();
});

it('stops with the library and ignores what a check of another library found', async () => {
  let answer: (marked: number) => void = () => {};
  vi.mocked(invoke).mockReturnValueOnce(new Promise((resolve) => (answer = resolve)));
  const found = vi.fn();
  const stop = watchXmpSidecars('/lib', found);
  await vi.advanceTimersByTimeAsync(0);
  useLibraryStore.setState({ sessionDirectory: '/other' });
  answer(3);
  await vi.advanceTimersByTimeAsync(0);
  expect(found).not.toHaveBeenCalled();
  stop();
  window.dispatchEvent(new Event('focus'));
  useLibraryStore.setState({ currentFolderPath: '/lib/Selects' });
  await vi.advanceTimersByTimeAsync(2000);
  expect(checks()).toHaveLength(1);
});
