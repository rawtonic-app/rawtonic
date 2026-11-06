import { beforeEach, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { changeLibraryAlbums, type AlbumChange } from './libraryAlbums';
import { applyMockAlbumChange } from './libraryAlbums.testSupport';
import { Invokes } from '../ui/AppProperties';
import { libraryChanges } from '../../store/libraryFiles';
import type { AlbumItem } from '../ui/AppProperties';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/plugin-os', () => ({ platform: () => 'macos' }));
vi.mock('../../store/useLibraryStore', () => ({ useLibraryStore: { getState: () => ({ setLibrary: vi.fn() }) } }));

const album = (id: string, images: string[] = []): AlbumItem => ({ type: 'album', id, name: id, images });

/** The backend's album file: every change is read-modify-write of it, slow
 * enough for the next call to be made while it runs. `addToAlbum` is the
 * backend's own change (`library_change`), applied to the album as it is. */
function backend(start: AlbumItem[]) {
  let stored = start;
  vi.mocked(invoke).mockImplementation((async (command: Invokes, args: Record<string, unknown> = {}) => {
    if (command === Invokes.GetAlbums) {
      const tree = structuredClone(stored);
      await new Promise((resolve) => setTimeout(resolve, 5));
      return tree;
    }
    await new Promise((resolve) => setTimeout(resolve, 5));
    if (command === Invokes.SaveAlbums) stored = structuredClone(args.tree as AlbumItem[]);
    if (command === Invokes.LibraryChange) {
      stored = applyMockAlbumChange(stored, args.change as AlbumChange);
    }
    return structuredClone(stored);
  }) as typeof invoke);
  return {
    stored: () => stored,
    /** A file operation's own change of the backend's list. */
    addAlbum: (item: AlbumItem) => (stored = [...stored, item])
  };
}

beforeEach(() => {
  vi.clearAllMocks();
  libraryChanges.reset();
});

it('two rapid additions both survive: the second starts from the list the first saved', async () => {
  // Round 1 (2d84b4f5): both read the stored list before the first save entered
  // its queue, and the second snapshot wrote the first addition away.
  const { stored } = backend([album('Existing')]);
  const add = (id: string) => changeLibraryAlbums({ kind: 'create', item: album(id), parent: null });
  const [first, second] = await Promise.all([add('First'), add('Second')]);
  const ids = () => stored().map((item) => item.id);
  expect(ids()).toEqual(['Existing', 'First', 'Second']);
  expect(second.map((item) => item.id)).toContain('First');
  expect(first.map((item) => item.id)).toEqual(['Existing', 'First']);
});

it('two rapid additions to an album from a menu both survive', async () => {
  const { stored } = backend([album('Trip', ['a.dng'])]);
  const add = (path: string) => changeLibraryAlbums({ kind: 'addToAlbum', albumId: 'Trip', paths: [path] });
  await Promise.all([add('b.dng'), add('c.dng')]);
  expect((stored()[0] as { images: string[] }).images).toEqual(['a.dng', 'b.dng', 'c.dng']);
});

it('a photograph added to an album lands in it as the backend has it, keeping an album added meanwhile', async () => {
  // A file operation adds `New` to the backend's list while this page's
  // addition to `Trip` is on its way.
  const { stored, addAlbum } = backend([album('Trip', ['a.dng'])]);
  const added = changeLibraryAlbums({ kind: 'addToAlbum', albumId: 'Trip', paths: ['b.dng'] });
  addAlbum(album('New'));
  await added;
  expect(stored().map((item) => item.id)).toEqual(['Trip', 'New']);
  expect((stored()[0] as { images: string[] }).images).toEqual(['a.dng', 'b.dng']);
  expect(vi.mocked(invoke).mock.calls.some(([command]) => command === Invokes.LibraryChange)).toBe(true);
});

it('adding one photo never sends a photo trashed after the old list was read', async () => {
  let stored: AlbumItem[] = [album('Trip', ['trashed.dng'])];
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    if (command === Invokes.GetAlbums) {
      const old = structuredClone(stored);
      stored = [album('Trip')]; // The file operation finishes after this read.
      return old;
    }
    if (command === Invokes.LibraryChange) {
      const { paths } = (args as { change: { paths: string[] } }).change;
      expect(paths).toEqual(['new.dng']);
      stored = [album('Trip', paths)];
    }
    return stored;
  });
  await changeLibraryAlbums({ kind: 'addToAlbum', albumId: 'Trip', paths: ['trashed.dng', 'new.dng'] });
  expect(stored).toEqual([album('Trip', ['new.dng'])]);
});

it.each<AlbumChange>([
  { kind: 'rename', id: 'Trip', name: 'Renamed' },
  { kind: 'delete', id: 'Other' },
  { kind: 'move', id: 'Trip', parent: 'group', index: 0 },
  { kind: 'removePhotos', albumId: 'Trip', paths: ['remove.dng'] }
])('$kind sends a backend change instead of overwriting a file operation', async (change) => {
  const current: AlbumItem[] = [
    album('Trip', ['renamed.dng', 'remove.dng']),
    album('Other'),
    { type: 'group', id: 'group', name: 'Group', children: [] }
  ];
  let result: AlbumItem[] = [];
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    if (command === Invokes.LibraryChange)
      result = applyMockAlbumChange(current, (args as { change: AlbumChange }).change);
    return result;
  });
  await changeLibraryAlbums(change);
  expect(invoke).toHaveBeenCalledExactlyOnceWith(Invokes.LibraryChange, { change });
  expect(JSON.stringify(result)).toContain('renamed.dng');
});
