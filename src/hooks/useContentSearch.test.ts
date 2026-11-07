import '../i18n';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { act, cleanup, renderHook } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { CONTENT_DELAY_MS, CONTENT_PROGRESS_RERANK_MS, useContentSearch } from './useContentSearch';
import { useLibraryStore } from '../store/useLibraryStore';
import { useSettingsStore } from '../store/useSettingsStore';
import { Invokes, type AppSettings, type ImageFile } from '../components/ui/AppProperties';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
const { handlers } = vi.hoisted(() => ({ handlers: new Map<string, (e: { payload: unknown }) => void>() }));
vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn((event: string, handler: (e: { payload: unknown }) => void) => {
    handlers.set(event, handler);
    return Promise.resolve(() => {});
  })
}));
const { toast } = vi.hoisted(() => ({ toast: { warn: vi.fn(), error: vi.fn() } }));
vi.mock('../activity/notify', () => ({ notify: toast }));
const mockInvoke = vi.mocked(invoke);
const calls = (command: string) => mockInvoke.mock.calls.filter(([c]) => c === command);
const answer = {
  paths: ['/Job/Capture/B.ARW', '/Job/Capture/A.ARW'],
  indexed: 2,
  failed: 0,
  total: 3
};

beforeEach(() => {
  vi.useFakeTimers();
  localStorage.clear();
  toast.error.mockReset();
  mockInvoke.mockReset();
  mockInvoke.mockImplementation(async (command) => (command === Invokes.ContentSearchRank ? answer : undefined));
  useSettingsStore.setState({ appSettings: { contentSearch: true } as AppSettings });
  useLibraryStore.setState({
    sessionDirectory: '/Job',
    currentFolderPath: '/Job/Capture',
    activeAlbumId: null,
    imageList: [{ path: '/Job/Capture/A.ARW' } as ImageFile],
    searchCriteria: { text: '', tags: [], mode: 'OR' },
    contentRanking: null
  });
});
afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

const type = (text: string) => act(() => useLibraryStore.getState().setSearchCriteria({ text }));

it('takes the mode remembered for the library and ranks only in Content mode', async () => {
  localStorage.setItem('rawtonic.search.mode:/Job', 'content');
  renderHook(() => useContentSearch());
  expect(useLibraryStore.getState().searchMode).toBe('content');
  expect(mockInvoke).toHaveBeenCalledWith(Invokes.StartContentIndex, { directory: '/Job', resume: true });
  type('A Heron ');
  await act(async () => vi.advanceTimersByTimeAsync(CONTENT_DELAY_MS - 1));
  expect(calls(Invokes.ContentSearchRank)).toHaveLength(0);
  await act(async () => vi.advanceTimersByTimeAsync(1));
  expect(calls(Invokes.ContentSearchRank)).toEqual([
    [Invokes.ContentSearchRank, { directory: '/Job', query: 'A Heron', listed: '/Job/Capture' }]
  ]);
  const ranks = useLibraryStore.getState().contentRanking?.ranks;
  expect([...(ranks ?? [])]).toEqual([
    ['/Job/Capture/B.ARW', 0],
    ['/Job/Capture/A.ARW', 1]
  ]);
  // Text mode: the text filters again, no ranking.
  act(() => useLibraryStore.setState({ searchMode: 'text' }));
  await act(async () => vi.advanceTimersByTimeAsync(CONTENT_DELAY_MS));
  expect(useLibraryStore.getState().contentRanking).toBeNull();
  expect(calls(Invokes.ContentSearchRank)).toHaveLength(1);
});

it('without the model asks for the status (the switch offers the download) instead of an error', async () => {
  localStorage.setItem('rawtonic.search.mode:/Job', 'content');
  mockInvoke.mockImplementation(async (command) => {
    if (command === Invokes.ContentSearchRank) throw { code: 'models_missing', message: 'not installed' };
    if (command === Invokes.ContentSearchStatus)
      return { installed: false, downloadBytes: 1, running: false, indexed: 0, failed: 0, total: 3 };
    return undefined;
  });
  renderHook(() => useContentSearch());
  type('a dog');
  await act(async () => vi.advanceTimersByTimeAsync(CONTENT_DELAY_MS));
  expect(toast.error).not.toHaveBeenCalled();
  expect(useLibraryStore.getState().contentStatus?.installed).toBe(false);
  expect(useLibraryStore.getState().contentRanking).toBeNull();
});

it('does not index while the setting is off', () => {
  useSettingsStore.setState({ appSettings: {} as AppSettings });
  renderHook(() => useContentSearch());
  expect(calls(Invokes.StartContentIndex)).toHaveLength(0);
});

it('asks again when the list changes (a rename) and while the pass indexes', async () => {
  localStorage.setItem('rawtonic.search.mode:/Job', 'content');
  renderHook(() => useContentSearch());
  type('a dog');
  await act(async () => vi.advanceTimersByTimeAsync(CONTENT_DELAY_MS));
  expect(calls(Invokes.ContentSearchRank)).toHaveLength(1);
  // A rename gives a new list with the new path: ranked again.
  act(() => useLibraryStore.setState({ imageList: [{ path: '/Job/Capture/Renamed.ARW' } as ImageFile] }));
  await act(async () => vi.advanceTimersByTimeAsync(CONTENT_DELAY_MS));
  expect(calls(Invokes.ContentSearchRank)).toHaveLength(2);
  // Progress of the pass: at most one new ranking per interval.
  const progress = handlers.get('content-index-progress')!;
  const step = (indexed: number) =>
    act(() => progress({ payload: { directory: '/Job', indexed, failed: 0, total: 3 } }));
  await act(async () => vi.advanceTimersByTimeAsync(CONTENT_PROGRESS_RERANK_MS));
  step(2);
  step(3);
  await act(async () => vi.advanceTimersByTimeAsync(CONTENT_DELAY_MS));
  expect(calls(Invokes.ContentSearchRank)).toHaveLength(3);
});
