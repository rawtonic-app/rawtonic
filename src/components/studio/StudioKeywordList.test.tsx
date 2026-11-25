import '../../i18n';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { ContextMenuProvider } from '../../context/ContextMenuContext';
import { useLibraryStore } from '../../store/useLibraryStore';
import type { ImageFile } from '../ui/AppProperties';
import { StudioKeywordList, plannedRename } from './StudioKeywordList';
import { buildKeywordTree, typedKeyword } from '../../utils/keywordTree';
import { getUndoLabel, redo, undo, useCatalogUndoStore } from '../../utils/appUndo';
import { TILE_DROP_EVENT } from './tileDrag';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('../../activity/notify', () => ({
  notify: { error: vi.fn(), info: vi.fn(), dismiss: vi.fn(), update: vi.fn(), isActive: vi.fn(() => false) }
}));
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
    // A virtual copy no view lists, with keywords of its own.
    if (command === 'session_keyword_copies') return ['/job/Other/d.arw?vc=a1b2c3'];
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
beforeEach(() => useCatalogUndoStore.setState(useCatalogUndoStore.getInitialState()));
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
    expect(mockInvoke).toHaveBeenCalledWith('session_keyword_copies', { directory: '/job', path: 'Animals' });
    expect(mockInvoke).toHaveBeenCalledWith('rename_keyword_for_paths', {
      paths: ['/job/Capture/a.arw', '/job/Capture/b.arw', '/job/Other/d.arw?vc=a1b2c3'],
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

describe('Keyword List: photos dropped on a keyword, keywords without photos, delete', () => {
  /** The library has `Animals|Birds` and `Unused` without photographs. */
  const withDeclared = () => {
    const base = mockInvoke.getMockImplementation()!;
    mockInvoke.mockImplementation(async (command, args) => {
      if (command === 'session_keywords_declared') return ['Animals|Birds', 'Unused'];
      return base(command, args);
    });
  };

  it('lists a keyword without photographs with a count of 0', async () => {
    withDeclared();
    await renderList();
    expect(screen.getByText('Unused').closest('[role="treeitem"]')?.textContent).toContain('0');
    fireEvent.click(screen.getByLabelText('Show the keywords under Animals'));
    expect(screen.getByText('Birds').closest('[role="treeitem"]')?.textContent).toContain('0');
  });

  it('adds a keyword to the photos dropped on it as one undoable step', async () => {
    await renderList();
    const dropped = ['/job/Capture/a.arw', '/job/Capture/c.arw'];
    mockInvoke.mockImplementation(async (command, args) => {
      if (command === 'add_tag_for_paths')
        return [{ path: '/job/Capture/c.arw', added: [(args as { tag: string }).tag], removed: [] }];
      if (command === 'session_facets') return { keyword: facets };
      return undefined;
    });
    fireEvent.click(screen.getByLabelText('Show the keywords under Animals'));
    const row = screen.getByText('Cats').closest('[role="treeitem"]')!;
    await act(async () => {
      row.dispatchEvent(
        new CustomEvent(TILE_DROP_EVENT, { bubbles: true, detail: { target: 'Animals|Cats', paths: dropped } })
      );
    });
    expect(mockInvoke).toHaveBeenCalledWith('add_tag_for_paths', { paths: dropped, tag: 'user:Animals|Cats' });
    const tags = Object.fromEntries(useLibraryStore.getState().imageList.map((f) => [f.path, f.tags]));
    expect(tags['/job/Capture/c.arw']).toEqual(['user:Animals|Cats', 'user:beach']);
    expect(getUndoLabel()).toBe('Undo Add Keyword “Animals > Cats”');
    await act(async () => {
      await undo();
    });
    expect(mockInvoke).toHaveBeenCalledWith('restore_keywords', {
      changes: [{ path: '/job/Capture/c.arw', added: ['user:Animals|Cats'], removed: [] }],
      undo: true
    });
  });

  it('makes a keyword inside another without photographs, undoable', async () => {
    await renderList();
    mockInvoke.mockImplementation(async (command) => {
      if (command === 'session_facets') return { keyword: facets };
      if (command === 'session_keyword_declare') return { added: ['Animals|Birds'], removed: [] };
      return undefined;
    });
    fireEvent.contextMenu(screen.getByText('Animals'));
    fireEvent.click(await screen.findByText('New Keyword Inside “Animals”…'));
    const input = screen.getByLabelText('New keyword inside Animals');
    fireEvent.change(input, { target: { value: 'Birds' } });
    await act(async () => {
      fireEvent.keyDown(input, { key: 'Enter' });
    });
    expect(mockInvoke).toHaveBeenCalledWith('session_keyword_declare', { directory: '/job', path: 'Animals|birds' });
    expect(getUndoLabel()).toBe('Undo New Keyword');
    await act(async () => {
      await undo();
    });
    expect(mockInvoke).toHaveBeenCalledWith('session_keywords_declared_apply', {
      directory: '/job',
      remove: ['Animals|Birds'],
      add: []
    });
  });

  it('makes a top-level keyword with the name of one inside another, as asked', async () => {
    await renderList();
    mockInvoke.mockImplementation(async (command) => {
      if (command === 'session_facets') return { keyword: facets };
      if (command === 'session_keyword_declare') return { added: ['dogs'], removed: [] };
      return undefined;
    });
    fireEvent.click(screen.getByRole('button', { name: 'New Keyword…' }));
    fireEvent.click(await screen.findByText('New Keyword…'));
    const input = screen.getByLabelText(/New keyword:/);
    fireEvent.change(input, { target: { value: 'Dogs' } });
    await act(async () => {
      fireEvent.keyDown(input, { key: 'Enter' });
    });
    // Not `Animals|Dogs`: the keyword list's `Dogs` is no entry shorthand.
    expect(mockInvoke).toHaveBeenCalledWith('session_keyword_declare', { directory: '/job', path: 'dogs' });
  });

  it('takes the keyword filter back with an undone rename', async () => {
    await renderList();
    fireEvent.click(screen.getByText('Animals'));
    expect(useLibraryStore.getState().filterCriteria.metadata?.keyword).toEqual(['Animals']);
    fireEvent.doubleClick(screen.getByText('Animals'));
    const input = screen.getByLabelText(/Rename or move Animals/) as HTMLInputElement;
    fireEvent.change(input, { target: { value: 'Pets' } });
    fireEvent.keyDown(input, { key: 'Enter' });
    const base = mockInvoke.getMockImplementation()!;
    mockInvoke.mockImplementation(async (command, args) =>
      command === 'rename_keyword_for_paths'
        ? [{ path: '/job/Capture/a.arw', added: ['user:Pets|Dogs'], removed: ['user:Animals|Dogs'] }]
        : base(command, args)
    );
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: 'Rename' }));
    });
    expect(useLibraryStore.getState().filterCriteria.metadata?.keyword).toEqual(['Pets']);
    await act(async () => {
      await undo();
    });
    expect(useLibraryStore.getState().filterCriteria.metadata?.keyword).toEqual(['Animals']);
    await act(async () => {
      await redo();
    });
    expect(useLibraryStore.getState().filterCriteria.metadata?.keyword).toEqual(['Pets']);
  });

  it('deletes a keyword after a confirmation naming the photos; its children move up', async () => {
    await renderList();
    const base = mockInvoke.getMockImplementation()!;
    mockInvoke.mockImplementation(async (command, args) =>
      command === 'delete_keyword_for_paths'
        ? [{ path: '/job/Capture/b.arw', added: ['user:Dogs|Beagle'], removed: ['user:Animals|Dogs|Beagle'] }]
        : base(command, args)
    );
    fireEvent.contextMenu(screen.getByText('Animals'));
    fireEvent.click(await screen.findByText('Delete Keyword…'));
    expect(screen.getByRole('alertdialog').textContent).toContain(
      'Delete “Animals”? Keywords under it move up one level. This changes the keywords of 3 photos.'
    );
    expect(mockInvoke).not.toHaveBeenCalledWith('delete_keyword_for_paths', expect.anything());
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: 'Delete' }));
    });
    expect(mockInvoke).toHaveBeenCalledWith('delete_keyword_for_paths', expect.objectContaining({ path: 'Animals' }));
    const tags = Object.fromEntries(useLibraryStore.getState().imageList.map((f) => [f.path, f.tags]));
    expect(tags['/job/Capture/a.arw']).toEqual(['color:red', 'user:Dogs']);
    expect(tags['/job/Capture/b.arw']).toEqual(['user:Dogs|Beagle']);
    expect(getUndoLabel()).toBe('Undo Delete Keyword');
  });

  it('deletes the unused keywords after a confirmation, as one undoable step', async () => {
    withDeclared();
    await renderList();
    fireEvent.click(screen.getByRole('button', { name: 'New Keyword…' }));
    fireEvent.click(await screen.findByText('Delete Unused Keywords…'));
    expect(screen.getByRole('alertdialog').textContent).toContain('Delete every keyword that no photo has?');
    mockInvoke.mockImplementation(async (command) =>
      command === 'session_keywords_declared_apply' ? { added: [], removed: ['Animals|Birds', 'Unused'] } : undefined
    );
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: 'Delete' }));
    });
    const call = mockInvoke.mock.calls.find(([command]) => command === 'session_keywords_declared_apply')!;
    expect((call[1] as { remove: string[] }).remove.sort()).toEqual(['Animals|Birds', 'Unused']);
    expect(getUndoLabel()).toBe('Undo Delete Unused Keywords');
  });
});

describe('Keyword List outside a library', () => {
  it('is not shown: filtering by a keyword is the catalog’s', async () => {
    useLibraryStore.setState({ sessionDirectory: null });
    render(
      <ContextMenuProvider>
        <StudioKeywordList />
      </ContextMenuProvider>
    );
    await act(async () => {});
    expect(screen.queryByText('Keyword List')).toBeNull();
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

  it('leaves a name holding `>` or `,` as it is when its field is confirmed unchanged', () => {
    // The field starts with `typedKeyword` (KWC1: imported literal names).
    const literal = buildKeywordTree([
      { value: 'People|A > B', count: 1 },
      { value: 'Parent|comma,term', count: 1 }
    ]);
    expect(plannedRename(literal, 'People|A > B', typedKeyword('People|A > B'))).toBeNull();
    expect(plannedRename(literal, 'Parent|comma,term', typedKeyword('Parent|comma,term'))).toBeNull();
  });
});
