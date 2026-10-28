import '../i18n';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { act, cleanup, renderHook } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { useCatalogSearch, SEARCH_DELAY_MS } from './useCatalogSearch';
import { toast } from 'react-toastify';
import { useLibraryStore } from '../store/useLibraryStore';
import { catalogChanged } from '../utils/catalogWrites';
import { IMPORTS_CHANGED } from '../utils/folderSync';
import { OFFLINE_CHANGED } from '../utils/offline';
import { RawStatus } from '../components/ui/AppProperties';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('react-toastify', () => ({ toast: { error: vi.fn() } }));
const listeners = new Map<string, () => void>();
vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(async (name: string, handler: () => void) => {
    listeners.set(name, handler);
    return () => listeners.delete(name);
  }),
}));
const mockInvoke = vi.mocked(invoke);
/** Searches asked, leaving out the question which photographs the catalog
 * knows (asked with each search, `catalogKnown`). */
const searches = () =>
  mockInvoke.mock.calls.filter(([, args]) => JSON.stringify((args as { rule: unknown }).rule) !== '{"combine":"all","rules":[]}')
    .length;

beforeEach(() => {
  vi.useFakeTimers();
  mockInvoke.mockReset();
  mockInvoke.mockResolvedValue(['/job/Capture/a.arw']);
  useLibraryStore.setState({
    sessionDirectory: '/job',
    searchCriteria: { text: '', tags: [], mode: 'OR' },
    filterCriteria: { colors: [], rating: 0, rawStatus: RawStatus.All },
    catalogMatches: null,
  });
});
afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

const settle = async () => {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(SEARCH_DELAY_MS + 1);
  });
};

it('asks the catalog after typing pauses and again after a catalog write', async () => {
  renderHook(() => useCatalogSearch());
  await settle();
  // Nothing to search: no query, the grid filters by itself.
  expect(mockInvoke).not.toHaveBeenCalled();
  expect(useLibraryStore.getState().catalogMatches).toBeNull();

  act(() => useLibraryStore.getState().setSearchCriteria({ text: 'we' }));
  act(() => useLibraryStore.getState().setSearchCriteria({ text: 'wedding' }));
  await settle();
  expect(searches()).toBe(1);
  expect(mockInvoke).toHaveBeenCalledWith('session_search', {
    directory: '/job',
    listed: null,
    rule: { field: 'text', op: 'contains', value: 'wedding' },
  });
  expect([...(useLibraryStore.getState().catalogMatches ?? [])]).toEqual(['/job/Capture/a.arw']);

  mockInvoke.mockResolvedValue([]);
  act(() => catalogChanged());
  await settle();
  expect(searches()).toBe(2);
  expect(useLibraryStore.getState().catalogMatches?.size).toBe(0);

  act(() => useLibraryStore.getState().setFilterCriteria({ metadata: { camera: ['X-T4'] } }));
  await settle();
  expect(mockInvoke).toHaveBeenCalledWith('session_search', {
    directory: '/job',
    listed: null,
    rule: {
      combine: 'all',
      rules: [
        { field: 'text', op: 'contains', value: 'wedding' },
        { field: 'camera', op: 'is', value: 'X-T4' },
      ],
    },
  });

  act(() => useLibraryStore.getState().setLibrary({ sessionDirectory: null }));
  await settle();
  expect(useLibraryStore.getState().catalogMatches).toBeNull();
});

it('drops an answer for a library closed meanwhile', async () => {
  let answer: (paths: string[]) => void = () => {};
  mockInvoke.mockImplementation(() => new Promise((resolve) => (answer = resolve as typeof answer)));
  useLibraryStore.setState({ searchCriteria: { text: 'x', tags: [], mode: 'OR' } });
  renderHook(() => useCatalogSearch());
  await settle();
  act(() => useLibraryStore.setState({ sessionDirectory: '/other' }));
  await act(async () => answer(['/job/Capture/a.arw']));
  expect(useLibraryStore.getState().catalogMatches).toBeNull();
});

it('searches again when the background pass recorded capture facts', async () => {
  useLibraryStore.setState({ searchCriteria: { text: 'X-T4', tags: [], mode: 'OR' } });
  const { unmount } = renderHook(() => useCatalogSearch());
  await settle();
  expect(searches()).toBe(1);
  act(() => listeners.get('library-facts-recorded')?.());
  await settle();
  expect(searches()).toBe(2);
  unmount();
  expect(listeners.has('library-facts-recorded')).toBe(false);
});

it('keeps the newest answer when an older search answers late', async () => {
  const answers: Array<(paths: string[]) => void> = [];
  mockInvoke.mockImplementation(() => new Promise((resolve) => answers.push(resolve as (p: string[]) => void)));
  useLibraryStore.setState({ searchCriteria: { text: 'bride', tags: [], mode: 'OR' } });
  renderHook(() => useCatalogSearch());
  await settle();
  act(() => catalogChanged());
  await settle();
  // Two rounds of two questions (matches, known photographs).
  expect(answers).toHaveLength(4);
  await act(async () => {
    answers[2](['/job/new.arw']);
    answers[3](['/job/new.arw']);
  });
  await act(async () => {
    answers[0](['/job/old.arw']);
    answers[1](['/job/old.arw']);
  });
  expect([...(useLibraryStore.getState().catalogMatches ?? [])]).toEqual(['/job/new.arw']);
});

it('searches again after an import, a tethered capture or a relink', async () => {
  useLibraryStore.setState({ searchCriteria: { text: 'bride', tags: [], mode: 'OR' } });
  renderHook(() => useCatalogSearch());
  await settle();
  const before = searches();
  act(() => window.dispatchEvent(new Event(IMPORTS_CHANGED)));
  await settle();
  act(() => window.dispatchEvent(new Event(OFFLINE_CHANGED)));
  await settle();
  expect(searches()).toBe(before + 2);
});

it('asks again when the grid gets a new list (a rename or a refresh)', async () => {
  useLibraryStore.setState({ searchCriteria: { text: 'bride', tags: [], mode: 'OR' } });
  renderHook(() => useCatalogSearch());
  await settle();
  const before = searches();
  act(() => useLibraryStore.setState({ imageList: [] }));
  await settle();
  expect(searches()).toBe(before + 1);
});

it('shows nothing, and says so, when a search fails after an earlier one worked', async () => {
  useLibraryStore.setState({ searchCriteria: { text: 'bride', tags: [], mode: 'OR' } });
  renderHook(() => useCatalogSearch());
  await settle();
  expect(useLibraryStore.getState().catalogMatches?.size).toBe(1);
  mockInvoke.mockRejectedValue('database is locked');
  act(() => useLibraryStore.getState().setSearchCriteria({ text: 'groom' }));
  await settle();
  expect(useLibraryStore.getState().catalogMatches?.size).toBe(0);
  expect(vi.mocked(toast.error)).toHaveBeenCalledWith(expect.stringContaining('database is locked'));
});

it('asks for answers in the form of the folder the grid lists', async () => {
  useLibraryStore.setState({
    searchCriteria: { text: 'bride', tags: [], mode: 'OR' },
    currentFolderPath: '/alias/Capture',
    activeAlbumId: null,
  });
  renderHook(() => useCatalogSearch());
  await settle();
  expect(mockInvoke).toHaveBeenCalledWith('session_search', {
    directory: '/job',
    listed: '/alias/Capture',
    rule: { field: 'text', op: 'contains', value: 'bride' },
  });
  useLibraryStore.setState({ currentFolderPath: null });
});

it('refreshes AI searches after recipe events and removes the listener on close', async () => {
  useLibraryStore.setState({ filterCriteria: { colors: [], rating: 0, rawStatus: RawStatus.All, metadata: { ai: ['generated'] } } });
  const { unmount } = renderHook(() => useCatalogSearch());
  await settle();
  expect(useLibraryStore.getState().catalogMatches?.size).toBe(1);
  mockInvoke.mockResolvedValue([]);
  act(() => listeners.get('library-ai-facts-changed')?.());
  await settle();
  expect(useLibraryStore.getState().catalogMatches?.size).toBe(0);
  unmount();
  expect(listeners.has('library-ai-facts-changed')).toBe(false);
});
