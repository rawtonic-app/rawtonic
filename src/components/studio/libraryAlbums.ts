import { invoke } from '@tauri-apps/api/core';
import { Invokes, type Album, type AlbumGroup, type AlbumItem } from '../ui/AppProperties';
import { useLibraryStore } from '../../store/useLibraryStore';
import { libraryChanges } from '../../store/libraryFiles';
import type { SessionAlbum } from './StudioAlbums';

/** The app-wide albums (`albums.json`, outside sessions) as the Albums tool
 * lists them: depth first, a group before its children, in stored order. */
export function flattenAlbumTree(tree: AlbumItem[], parent: string | null = null, out: SessionAlbum[] = []) {
  tree.forEach((item, position) => {
    const group = item.type === 'group';
    out.push({
      id: item.id,
      name: item.name,
      parent,
      count: group ? 0 : (item as Album).images.length,
      kind: group ? 'group' : 'album',
      position
    });
    if (group) flattenAlbumTree((item as AlbumGroup).children, item.id, out);
  });
  return out;
}

export function findAlbumItem(tree: AlbumItem[], id: string): AlbumItem | null {
  for (const item of tree) {
    if (item.id === id) return item;
    if (item.type === 'group') {
      const found = findAlbumItem(item.children, id);
      if (found) return found;
    }
  }
  return null;
}

/** A single edit to albums.json, applied under the backend store's lock.
 * The discriminants and camelCase fields are pinned by the shared IPC contracts. */
export type AlbumChange =
  | { kind: 'addToAlbum' | 'removePhotos'; albumId: string; paths: string[] }
  | { kind: 'rename'; id: string; name: string }
  | { kind: 'delete'; id: string }
  | { kind: 'move'; id: string; parent: string | null; index: number }
  | { kind: 'create'; item: AlbumItem; parent: string | null };

/** Send only the user's edit. File operations can update album members while
 * this command is on its way; the backend applies it to the current tree. */
export async function changeLibraryAlbums(change: AlbumChange): Promise<AlbumItem[]> {
  const saved = await libraryChanges.enqueue(async () => {
    if (change.kind === 'addToAlbum') {
      // A selection can include existing members. Send only its additions,
      // so trashing an old member after this read cannot put it back.
      const stored = (await invoke<AlbumItem[]>(Invokes.GetAlbums)) ?? [];
      const album = findAlbumItem(stored, change.albumId);
      if (album?.type === 'album') {
        change = { ...change, paths: change.paths.filter((path) => !album.images.includes(path)) };
      }
    }
    return invoke<AlbumItem[]>(Invokes.LibraryChange, { change });
  });
  useLibraryStore.getState().setLibrary({ albumTree: saved });
  return saved;
}

export async function loadLibraryAlbums() {
  const tree = (await invoke<AlbumItem[]>(Invokes.GetAlbums)) ?? [];
  useLibraryStore.getState().setLibrary({ albumTree: tree });
  return tree;
}
