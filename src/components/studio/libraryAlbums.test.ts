import { beforeEach, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { changeLibraryAlbums } from './libraryAlbums';
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
      const change = args.change as { kind: string; albumId: string; paths: string[] };
      stored = stored.map((item) =>
        item.type === 'album' && item.id === change.albumId
          ? { ...item, images: [...item.images, ...change.paths.filter((path) => !item.images.includes(path))] }
          : item
      );
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
  const add = (id: string) => changeLibraryAlbums((tree) => [...tree, album(id)]);
  const [first, second] = await Promise.all([add('First'), add('Second')]);
  const ids = () => stored().map((item) => item.id);
  expect(ids()).toEqual(['Existing', 'First', 'Second']);
  expect(second.map((item) => item.id)).toContain('First');
  expect(first.map((item) => item.id)).toEqual(['Existing', 'First']);
});

it('two rapid additions to an album from a menu both survive', async () => {
  const { stored } = backend([album('Trip', ['a.dng'])]);
  const add = (path: string) =>
    changeLibraryAlbums((tree) =>
      tree.map((item) => (item.type === 'album' ? { ...item, images: [...item.images, path] } : item))
    );
  await Promise.all([add('b.dng'), add('c.dng')]);
  expect((stored()[0] as { images: string[] }).images).toEqual(['a.dng', 'b.dng', 'c.dng']);
});

it('a photograph added to an album lands in it as the backend has it, keeping an album added meanwhile', async () => {
  // A file operation adds `New` to the backend's list while this page's
  // addition to `Trip` is on its way.
  const { stored, addAlbum } = backend([album('Trip', ['a.dng'])]);
  const added = changeLibraryAlbums((tree) =>
    tree.map((item) =>
      item.type === 'album' && item.id === 'Trip' ? { ...item, images: [...item.images, 'b.dng'] } : item
    )
  );
  addAlbum(album('New'));
  await added;
  expect(stored().map((item) => item.id)).toEqual(['Trip', 'New']);
  expect((stored()[0] as { images: string[] }).images).toEqual(['a.dng', 'b.dng']);
  expect(vi.mocked(invoke).mock.calls.some(([command]) => command === Invokes.LibraryChange)).toBe(true);
});
