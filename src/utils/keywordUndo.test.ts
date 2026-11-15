import '../i18n';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { debouncedSetHistory, useEditorStore } from '../store/useEditorStore';
import { useLibraryStore } from '../store/useLibraryStore';
import { useUIStore } from '../store/useUIStore';
import {
  canUndo,
  getRedoLabel,
  getUndoLabel,
  keywordAction,
  redo,
  undo,
  useCatalogUndoStore,
  type KeywordChange
} from './appUndo';
import type { ImageFile } from '../components/ui/AppProperties';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn(async () => []) }));
vi.mock('../activity/notify', () => ({
  notify: { error: vi.fn(), info: vi.fn(), dismiss: vi.fn(), update: vi.fn(), isActive: vi.fn(() => false) }
}));

const file = (path: string, tags: string[] | null): ImageFile =>
  ({ path, tags, rating: 0, modified: 0, is_edited: false, exif: null }) as unknown as ImageFile;

const dogs: KeywordChange[] = [
  { path: '/a.nef', added: ['user:Animals|Dogs'], removed: [] },
  { path: '/b.nef', added: ['user:Animals|Dogs'], removed: ['user:dogs'] }
];

beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(invoke).mockImplementation(async () => []);
  debouncedSetHistory.cancel();
  useEditorStore.setState(useEditorStore.getInitialState());
  useLibraryStore.setState({
    sessionDirectory: '/job',
    imageList: [file('/a.nef', ['user:Animals|Dogs']), file('/b.nef', ['user:Animals|Dogs', 'color:red'])]
  });
  useUIStore.setState({ activeView: 'library' });
  useCatalogUndoStore.setState(useCatalogUndoStore.getInitialState());
});

describe('keyword edits in the undo history', () => {
  it('is one step named for the keyword, undone and redone as written', async () => {
    await keywordAction({ kind: 'add', keyword: 'Animals > Dogs' }, async (record) => record({ changes: dogs }), [
      '/a.nef',
      '/b.nef'
    ]);
    expect(getUndoLabel()).toBe('Undo Add Keyword “Animals > Dogs”');
    await undo();
    expect(invoke).toHaveBeenLastCalledWith('restore_keywords', { changes: dogs, undo: true });
    const tags = Object.fromEntries(useLibraryStore.getState().imageList.map((image) => [image.path, image.tags]));
    // Gained keywords go, lost ones come back; the colour label stays.
    expect(tags['/a.nef']).toBeNull();
    expect(tags['/b.nef']).toEqual(['color:red', 'user:dogs']);
    expect(getRedoLabel()).toBe('Redo Add Keyword “Animals > Dogs”');
    await redo();
    expect(invoke).toHaveBeenLastCalledWith('restore_keywords', { changes: dogs, undo: false });
    expect(useLibraryStore.getState().imageList[1].tags).toEqual(['color:red', 'user:Animals|Dogs']);
  });

  it('undoes a Keyword List merge together with the keywords made without photographs', async () => {
    await keywordAction({ kind: 'merge', keyword: 'Fauna' }, async (record) => {
      record({ changes: dogs });
      record({ declared: { added: ['Animals'], removed: ['Fauna'] } });
    });
    expect(getUndoLabel()).toBe('Undo Merge Keywords');
    await undo();
    expect(invoke).toHaveBeenCalledWith('restore_keywords', { changes: dogs, undo: true });
    expect(invoke).toHaveBeenLastCalledWith('session_keywords_declared_apply', {
      directory: '/job',
      remove: ['Animals'],
      add: ['Fauna']
    });
    await redo();
    expect(invoke).toHaveBeenLastCalledWith('session_keywords_declared_apply', {
      directory: '/job',
      remove: ['Fauna'],
      add: ['Animals']
    });
  });

  it('names Rename, Move, Convert, Delete, New and Delete Unused Keywords', async () => {
    const declared = { added: ['Pets'], removed: [] };
    for (const [kind, label] of [
      ['rename', 'Undo Rename Keyword'],
      ['move', 'Undo Move Keyword'],
      ['convert', 'Undo Convert Keyword'],
      ['delete', 'Undo Delete Keyword'],
      ['new', 'Undo New Keyword'],
      ['deleteUnused', 'Undo Delete Unused Keywords']
    ] as const) {
      await keywordAction({ kind }, async (record) => record({ declared }));
      expect(getUndoLabel()).toBe(label);
    }
  });

  it('keeps no step for an edit that changed nothing', async () => {
    await keywordAction({ kind: 'add', keyword: 'Dogs' }, async () => {});
    expect(canUndo()).toBe(false);
  });

  it('keeps what saved of a write that failed for some photographs as one step', async () => {
    const failure = { message: 'Could not save 1 of 2 photos', failed: ['/b.nef'], keywords: [dogs[0]] };
    await expect(
      keywordAction({ kind: 'add', keyword: 'Dogs' }, async (record) => {
        record({ changes: failure.keywords });
        throw failure;
      })
    ).rejects.toBe(failure);
    await undo();
    expect(invoke).toHaveBeenLastCalledWith('restore_keywords', { changes: [dogs[0]], undo: true });
  });

  it('joins the changes of one photograph written twice (several keywords typed at once)', async () => {
    await keywordAction({ kind: 'add', keyword: 'a, b' }, async (record) => {
      record({ changes: [{ path: '/a.nef', added: ['user:a'], removed: [] }] });
      record({ changes: [{ path: '/a.nef', added: ['user:b'], removed: [] }] });
    });
    await undo();
    expect(invoke).toHaveBeenLastCalledWith('restore_keywords', {
      changes: [{ path: '/a.nef', added: ['user:a', 'user:b'], removed: [] }],
      undo: true
    });
  });
});
