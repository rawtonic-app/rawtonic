import '../../i18n';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { ContextMenuProvider } from '../../context/ContextMenuContext';
import { useLibraryStore } from '../../store/useLibraryStore';
import type { ImageFile } from '../ui/AppProperties';
import { StudioKeywordList, plannedRename } from './StudioKeywordList';
import { buildKeywordTree } from '../../utils/keywordTree';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('../../activity/notify', () => ({ notify: { error: vi.fn() } }));
const mockInvoke = vi.mocked(invoke);

const file = (path: string, tags: string[] | null): ImageFile => ({
  path,
  tags,
  rating: 0,
  modified: 0,
  is_edited: false,
  exif: null,
  is_virtual_copy: false,
  is_cloud_placeholder: false,
  is_raw: true,
  group_id: null
});

// The catalog's keyword list: every parent counted once per photograph.
const facets = [
  { value: 'Animals', count: 3 },
  { value: 'Animals|Dogs', count: 2 },
  { value: 'Animals|Cats', count: 1 },
  { value: 'places > asia', count: 1 },
  { value: 'Fauna', count: 1 }
];

beforeEach(() => {
  mockInvoke.mockReset();
  mockInvoke.mockImplementation(async (command) => {
    if (command === 'session_facets') return { keyword: [...facets, { value: '', count: 4 }] };
    if (command === 'session_search') return ['/job/Capture/a.arw', '/job/Capture/b.arw'];
    return undefined;
  });
  useLibraryStore.setState({
    sessionDirectory: '/job',
    imageList: [
      file('/job/Capture/a.arw', ['user:Animals|Dogs', 'color:red']),
      file('/job/Capture/b.arw', ['user:Animals|Dogs|Beagle']),
      file('/job/Capture/c.arw', ['user:beach'])
    ],
    filterCriteria: { ...useLibraryStore.getState().filterCriteria, metadata: {} }
  });
});
afterEach(cleanup);

const renderList = async () => {
  render(
    <ContextMenuProvider>
      <StudioKeywordList />
    </ContextMenuProvider>
  );
  await waitFor(() => expect(screen.getByText('Animals')).toBeTruthy());
};

describe('Keyword List', () => {
  it('shows the library’s keywords as a tree with counts, children on demand', async () => {
    await renderList();
    expect(mockInvoke).toHaveBeenCalledWith('session_facets', {
      directory: '/job',
      requests: [{ field: 'keyword', rule: { combine: 'all', rules: [] } }],
      listed: null
    });
    const animals = screen.getByText('Animals').closest('[role="treeitem"]')!;
    expect(animals.textContent).toContain('3');
    expect(screen.queryByText('Dogs')).toBeNull();
    fireEvent.click(screen.getByLabelText('Show the keywords under Animals'));
    expect(screen.getByText('Dogs').closest('[role="treeitem"]')?.getAttribute('aria-level')).toBe('2');
    expect(screen.getByText('Cats')).toBeTruthy();
  });

  it('filters the grid by a keyword, its children included, and takes the filter back', async () => {
    await renderList();
    fireEvent.click(screen.getByText('Animals'));
    // The catalog's keyword rule for `Animals` finds `Animals|Dogs` too
    // (crates/catalog/tests/search.rs: a_parent_keyword_finds…).
    expect(useLibraryStore.getState().filterCriteria.metadata?.keyword).toEqual(['Animals']);
    expect(screen.getByText('Animals').closest('[role="treeitem"]')?.getAttribute('aria-selected')).toBe('true');
    fireEvent.click(screen.getByText('Animals'));
    expect(useLibraryStore.getState().filterCriteria.metadata?.keyword).toEqual([]);
  });

  it('renames a keyword with the ones under it after a confirmation naming the photos', async () => {
    await renderList();
    fireEvent.doubleClick(screen.getByText('Animals'));
    const input = screen.getByLabelText(/Rename or move Animals/) as HTMLInputElement;
    expect(input.value).toBe('Animals');
    fireEvent.change(input, { target: { value: 'Pets' } });
    fireEvent.keyDown(input, { key: 'Enter' });
    expect(screen.getByRole('alertdialog').textContent).toContain(
      'Rename “Animals” to “Pets”? This changes the keywords of 3 photos.'
    );
    expect(mockInvoke).not.toHaveBeenCalledWith('rename_keyword_for_paths', expect.anything());
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: 'Rename' }));
    });
    expect(mockInvoke).toHaveBeenCalledWith('session_search', {
      directory: '/job',
      rule: { field: 'keyword', op: 'is', value: 'Animals' },
      listed: null
    });
    expect(mockInvoke).toHaveBeenCalledWith('rename_keyword_for_paths', {
      paths: ['/job/Capture/a.arw', '/job/Capture/b.arw'],
      from: 'Animals',
      to: 'Pets'
    });
    const tags = Object.fromEntries(useLibraryStore.getState().imageList.map((f) => [f.path, f.tags]));
    expect(tags['/job/Capture/a.arw']).toEqual(['color:red', 'user:Pets|Dogs']);
    expect(tags['/job/Capture/b.arw']).toEqual(['user:Pets|Dogs|Beagle']);
    expect(tags['/job/Capture/c.arw']).toEqual(['user:beach']);
  });

  it('offers a keyword typed as “a > b” before hierarchies for conversion', async () => {
    await renderList();
    fireEvent.contextMenu(screen.getByText('places > asia'));
    fireEvent.click(await screen.findByText('Convert to “places > asia”'));
    expect(screen.getByRole('alertdialog').textContent).toContain(
      'Convert “places > asia” into the hierarchy “places > asia”?'
    );
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: 'Convert' }));
    });
    expect(mockInvoke).toHaveBeenCalledWith(
      'rename_keyword_for_paths',
      expect.objectContaining({ from: 'places > asia', to: 'places|asia' })
    );
  });
});

describe('planned keyword renames', () => {
  const tree = buildKeywordTree(facets);
  it('merges into a keyword the library has, renames otherwise', () => {
    expect(plannedRename(tree, 'Animals|Cats', 'Animals > Dogs')).toMatchObject({
      kind: 'merge',
      to: 'Animals|Dogs',
      count: 1
    });
    expect(plannedRename(tree, 'Fauna', 'animals')).toMatchObject({ kind: 'merge', to: 'Animals' });
    expect(plannedRename(tree, 'Animals|Dogs', 'Dogs < Pets')).toMatchObject({ kind: 'rename', to: 'Pets|Dogs' });
    // Into itself is a rename of everything below, never a merge.
    expect(plannedRename(tree, 'Animals', 'Animals > Pets')).toMatchObject({ kind: 'rename', to: 'Animals|Pets' });
    expect(plannedRename(tree, 'Animals', ' Animals ')).toBeNull();
    // A change of case only is a rename.
    expect(plannedRename(tree, 'Fauna', 'fauna')).toMatchObject({ kind: 'rename', to: 'fauna' });
  });
});
