import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { act, cleanup, renderHook } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { useCatalogSearch, SEARCH_DELAY_MS } from './useCatalogSearch';
import { useLibraryStore } from '../store/useLibraryStore';
import { catalogChanged } from '../utils/catalogWrites';
import { RawStatus } from '../components/ui/AppProperties';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
const listeners = new Map<string, () => void>();
vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(async (name: string, handler: () => void) => {
    listeners.set(name, handler);
    return () => listeners.delete(name);
  }),
}));
const mockInvoke = vi.mocked(invoke);

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
  expect(mockInvoke).toHaveBeenCalledTimes(1);
  expect(mockInvoke).toHaveBeenCalledWith('session_search', {
    directory: '/job',
    rule: { field: 'text', op: 'contains', value: 'wedding' },
  });
  expect([...(useLibraryStore.getState().catalogMatches ?? [])]).toEqual(['/job/Capture/a.arw']);

  mockInvoke.mockResolvedValue([]);
  act(() => catalogChanged());
  await settle();
  expect(mockInvoke).toHaveBeenCalledTimes(2);
  expect(useLibraryStore.getState().catalogMatches?.size).toBe(0);

  act(() => useLibraryStore.getState().setFilterCriteria({ metadata: { camera: ['X-T4'] } }));
  await settle();
  expect(mockInvoke).toHaveBeenLastCalledWith('session_search', {
    directory: '/job',
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
  expect(mockInvoke).toHaveBeenCalledTimes(1);
  act(() => listeners.get('library-facts-recorded')?.());
  await settle();
  expect(mockInvoke).toHaveBeenCalledTimes(2);
  unmount();
  expect(listeners.has('library-facts-recorded')).toBe(false);
});
