import '../../i18n';
import { createRef } from 'react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { cleanup, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { invoke } from '@tauri-apps/api/core';
import StudioSearchBox from './StudioSearchBox';
import { useLibraryStore } from '../../store/useLibraryStore';
import { useSettingsStore } from '../../store/useSettingsStore';
import { Invokes, type AppSettings, type ImageFile } from '../ui/AppProperties';
import type { ContentSearchStatus } from '../../utils/contentSearch';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
const mockInvoke = vi.mocked(invoke);

const photo = (path: string) => ({ path }) as ImageFile;
const status = (fields: Partial<ContentSearchStatus>): ContentSearchStatus => ({
  installed: true,
  downloadBytes: 0,
  running: false,
  indexed: 10,
  failed: 0,
  total: 10,
  ...fields
});
const box = (files: ImageFile[] = []) => render(<StudioSearchBox inputRef={createRef()} files={files} />);

beforeEach(() => {
  localStorage.clear();
  mockInvoke.mockReset();
  mockInvoke.mockResolvedValue(status({}));
  useSettingsStore.setState({ appSettings: {} as AppSettings, handleSettingsChange: vi.fn() });
  useLibraryStore.setState({
    sessionDirectory: '/Shoots/Job',
    searchMode: 'text',
    contentRanking: null,
    contentStatus: null,
    searchCriteria: { text: '', tags: [], mode: 'OR' }
  });
});
afterEach(cleanup);

it('switches between Text and Content, remembered for the library', async () => {
  box();
  const text = screen.getByRole('button', { name: 'Text' });
  const content = screen.getByRole('button', { name: 'Content' });
  expect(text.getAttribute('aria-pressed')).toBe('true');
  expect(content.getAttribute('aria-pressed')).toBe('false');
  await userEvent.click(content);
  expect(useLibraryStore.getState().searchMode).toBe('content');
  expect(localStorage.getItem('rawtonic.search.mode:/Shoots/Job')).toBe('content');
  expect(screen.getByPlaceholderText('Describe what the photo shows')).toBeTruthy();
  await waitFor(() =>
    expect(mockInvoke).toHaveBeenCalledWith(Invokes.ContentSearchStatus, { directory: '/Shoots/Job' })
  );
});

it('has no Content mode outside a library', () => {
  useLibraryStore.setState({ sessionDirectory: null });
  box();
  expect(screen.queryByRole('button', { name: 'Content' })).toBeNull();
});

it('offers the model download with its size, and downloads only on the click', async () => {
  useLibraryStore.setState({
    searchMode: 'content',
    contentStatus: status({ installed: false, downloadBytes: 1_535_588_149, indexed: 0 })
  });
  box();
  const download = screen.getByRole('button', { name: 'Download search model (1.5 GB)' });
  expect(mockInvoke).not.toHaveBeenCalledWith(Invokes.ContentSearchDownload, expect.anything());
  mockInvoke.mockResolvedValue(undefined);
  await userEvent.click(download);
  expect(useSettingsStore.getState().handleSettingsChange).toHaveBeenCalledWith({ contentSearch: true });
  await waitFor(() =>
    expect(mockInvoke).toHaveBeenCalledWith(Invokes.ContentSearchDownload, { directory: '/Shoots/Job' })
  );
});

it('says how far the index is, and which shown photographs are not ranked yet', () => {
  useLibraryStore.setState({ searchMode: 'content', contentStatus: status({ indexed: 4213, total: 10000 }) });
  const { unmount } = box();
  expect(screen.getByRole('status').textContent).toBe('Indexed 4,213 of 10,000');
  unmount();
  useLibraryStore.setState({
    searchCriteria: { text: 'a heron', tags: [], mode: 'OR' },
    contentRanking: { query: 'a heron', ranks: new Map([['/Shoots/Job/Capture/A.ARW', 0]]) }
  });
  box([photo('/Shoots/Job/Capture/A.ARW'), photo('/Shoots/Job/Capture/B.ARW'), photo('/Shoots/Job/Capture/C.ARW')]);
  expect(screen.getByRole('status').textContent).toBe('2 photos not indexed yet, listed last');
});
